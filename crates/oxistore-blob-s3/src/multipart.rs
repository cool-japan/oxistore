//! S3 multipart upload support.
//!
//! Multipart upload allows uploading large objects in parts, with each part
//! individually signed and uploaded.  The sequence is:
//!
//! 1. [`S3BlobStore::create_multipart_upload`] — initiates the upload and
//!    returns an [`S3MultipartUpload`] handle containing the `upload_id`.
//! 2. [`S3MultipartUpload::upload_part`] — upload each part (≥5 MiB except
//!    the last).  The ETag returned by S3 is recorded internally.
//! 3. [`S3MultipartUpload::complete`] — commit the upload.  Parts are sorted
//!    by part number before assembling the `CompleteMultipartUpload` XML.
//! 4. [`S3MultipartUpload::abort`] — discard the upload if something goes wrong.
//!
//! # Example
//!
//! ```no_run
//! # use oxistore_blob_s3::{S3BlobStore, S3BlobStoreBuilder, S3Credentials};
//! # use bytes::Bytes;
//! # async fn example(store: S3BlobStore) -> Result<(), oxistore_blob::BlobError> {
//! let mut up = store.create_multipart_upload("big-file.bin").await?;
//! up.upload_part(1, Bytes::from(vec![0u8; 5 * 1024 * 1024])).await?;
//! up.upload_part(2, Bytes::from(vec![1u8; 1024])).await?;
//! up.complete().await?;
//! # Ok(())
//! # }
//! ```

use bytes::Bytes;
use oxistore_blob::BlobError;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::S3BlobStore;

/// Handle for an in-progress S3 multipart upload.
///
/// Created via [`S3BlobStore::create_multipart_upload`].
pub struct S3MultipartUpload<'a> {
    store: &'a S3BlobStore,
    key: String,
    upload_id: String,
    /// Accumulated (part_number, etag) pairs in upload order.
    parts: Vec<(u32, String)>,
}

impl S3BlobStore {
    /// Initiate a new multipart upload for the given key.
    ///
    /// Returns an [`S3MultipartUpload`] handle that must be either
    /// [`complete`]d or [`abort`]ed.
    ///
    /// [`complete`]: S3MultipartUpload::complete
    /// [`abort`]: S3MultipartUpload::abort
    pub async fn create_multipart_upload(
        &self,
        key: &str,
    ) -> Result<S3MultipartUpload<'_>, BlobError> {
        // POST /<bucket>/<key>?uploads
        let url = format!("{}?uploads", self.object_url(key)?);
        let resp = self.send("POST", &url, &[], &[]).await?;

        if resp.status != 200 {
            return Err(BlobError::MultipartError(format!(
                "CreateMultipartUpload failed with status {}",
                resp.status
            )));
        }

        let upload_id = parse_upload_id(&resp.body)?;

        Ok(S3MultipartUpload {
            store: self,
            key: key.to_string(),
            upload_id,
            parts: Vec::new(),
        })
    }
}

impl<'a> S3MultipartUpload<'a> {
    /// Upload a single part.
    ///
    /// `part_number` must be between 1 and 10000 (AWS S3 limit).
    /// Parts may be uploaded in any order; they are sorted by number during
    /// [`complete`](S3MultipartUpload::complete).
    pub async fn upload_part(&mut self, part_number: u32, body: Bytes) -> Result<(), BlobError> {
        // PUT /<key>?partNumber=<n>&uploadId=<id>
        let url = format!(
            "{}?partNumber={}&uploadId={}",
            self.store.object_url(&self.key)?,
            part_number,
            crate::percent_encode(&self.upload_id)
        );

        let resp = self.store.send("PUT", &url, body.as_ref(), &[]).await?;

        if resp.status != 200 && resp.status != 206 {
            return Err(BlobError::MultipartError(format!(
                "UploadPart({part_number}) failed with status {}",
                resp.status
            )));
        }

        // Capture ETag (may be quoted, pass through verbatim as AWS requires)
        let etag = resp.headers.get("etag").cloned().ok_or_else(|| {
            BlobError::MultipartError(format!("UploadPart({part_number}): missing ETag header"))
        })?;

        self.parts.push((part_number, etag));
        Ok(())
    }

    /// Complete the multipart upload.
    ///
    /// Parts are sorted by part number before the `CompleteMultipartUpload`
    /// request is sent.  AWS S3 requires parts to be listed in ascending
    /// part-number order in the XML payload.
    pub async fn complete(mut self) -> Result<(), BlobError> {
        // Sort parts in ascending part-number order (AWS requirement)
        self.parts.sort_by_key(|(n, _)| *n);

        let xml = build_complete_xml(&self.parts);

        // POST /<key>?uploadId=<id>
        let url = format!(
            "{}?uploadId={}",
            self.store.object_url(&self.key)?,
            crate::percent_encode(&self.upload_id)
        );

        let resp = self
            .store
            .send(
                "POST",
                &url,
                xml.as_bytes(),
                &[("content-type", "application/xml")],
            )
            .await?;

        if resp.status != 200 {
            return Err(BlobError::MultipartError(format!(
                "CompleteMultipartUpload failed with status {}: {}",
                resp.status,
                String::from_utf8_lossy(&resp.body)
            )));
        }

        Ok(())
    }

    /// Abort the multipart upload, freeing any uploaded parts on the server.
    pub async fn abort(self) -> Result<(), BlobError> {
        // DELETE /<key>?uploadId=<id>
        let url = format!(
            "{}?uploadId={}",
            self.store.object_url(&self.key)?,
            crate::percent_encode(&self.upload_id)
        );

        let resp = self.store.send("DELETE", &url, &[], &[]).await?;

        match resp.status {
            200 | 204 => Ok(()),
            _ => Err(BlobError::MultipartError(format!(
                "AbortMultipartUpload failed with status {}",
                resp.status
            ))),
        }
    }
}

// ── XML helpers ───────────────────────────────────────────────────────────────

/// Parse the `<UploadId>` element from a CreateMultipartUpload response.
fn parse_upload_id(xml: &[u8]) -> Result<String, BlobError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut in_upload_id = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == b"UploadId" => {
                in_upload_id = true;
            }
            Ok(Event::Text(e)) if in_upload_id => {
                let id = e.decode().map(|s| s.into_owned()).map_err(|err| {
                    BlobError::MultipartError(format!("XML decode UploadId: {err}"))
                })?;
                return Ok(id);
            }
            Ok(Event::End(_)) => {
                in_upload_id = false;
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(BlobError::MultipartError(format!(
                    "XML parse error in CreateMultipartUpload response: {e}"
                )))
            }
            _ => {}
        }
        buf.clear();
    }

    Err(BlobError::MultipartError(
        "CreateMultipartUpload response missing <UploadId>".to_string(),
    ))
}

/// Build the `CompleteMultipartUpload` XML body.
///
/// Parts are assumed to already be sorted by number (caller responsibility).
fn build_complete_xml(parts: &[(u32, String)]) -> String {
    let mut xml = String::from("<CompleteMultipartUpload>");
    for (num, etag) in parts {
        xml.push_str(&format!(
            "<Part><PartNumber>{num}</PartNumber><ETag>{etag}</ETag></Part>"
        ));
    }
    xml.push_str("</CompleteMultipartUpload>");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_xml_sorted_correctly() {
        let parts = vec![
            (1u32, "\"etag1\"".to_string()),
            (2u32, "\"etag2\"".to_string()),
            (3u32, "\"etag3\"".to_string()),
        ];
        let xml = build_complete_xml(&parts);
        assert!(xml.contains("<PartNumber>1</PartNumber>"));
        assert!(xml.contains("<ETag>\"etag1\"</ETag>"));
        // Verify order
        let pos1 = xml.find("<PartNumber>1</PartNumber>").expect("part 1");
        let pos2 = xml.find("<PartNumber>2</PartNumber>").expect("part 2");
        let pos3 = xml.find("<PartNumber>3</PartNumber>").expect("part 3");
        assert!(pos1 < pos2 && pos2 < pos3, "parts must be in order");
    }

    #[test]
    fn parse_upload_id_happy_path() {
        let xml = b"<?xml version=\"1.0\"?><InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key><UploadId>testid123</UploadId></InitiateMultipartUploadResult>";
        let id = parse_upload_id(xml).expect("parse upload id");
        assert_eq!(id, "testid123");
    }
}
