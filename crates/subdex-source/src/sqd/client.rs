//! Thin HTTP client for the SQD portal `/stream` endpoint.
//!
//! Responsibilities: build the JSON stream request (block range + field
//! selectors from [`DataSelection`]), POST it, read the finalized-head from the
//! response header, and parse the JSON-lines body into [`PortalBlock`]s. All
//! network calls surface [`SubdexError::Source`] so the shared retry layer treats
//! them as transient.

use crate::config::DataSelection;
use crate::sqd::mapping::PortalBlock;
use reqwest::Client;
use serde_json::json;
use subdex_core::{BlockNumber, Result, SubdexError};

/// Response header carrying the dataset's current finalized-head height.
const FINALIZED_HEAD_HEADER: &str = "x-sqd-finalized-head-number";

/// A configured connection to one SQD portal dataset.
pub(crate) struct PortalClient {
    http: Client,
    /// Full stream URL, e.g. `https://portal.sqd.dev/datasets/polkadot/stream`.
    stream_url: String,
}

impl PortalClient {
    /// Build a client for `portal_url` + `dataset`. `portal_url` is the portal
    /// base (e.g. `https://portal.sqd.dev`); the dataset is appended as
    /// `/datasets/{dataset}/stream`.
    pub(crate) fn new(portal_url: &str, dataset: &str) -> Result<Self> {
        let http = Client::builder()
            .build()
            .map_err(|e| SubdexError::Source(format!("build http client: {e}")))?;
        let base = portal_url.trim_end_matches('/');
        Ok(Self {
            http,
            stream_url: format!("{base}/datasets/{dataset}/stream"),
        })
    }

    /// The dataset's current finalized head. Queries a 1-block stream at height 0
    /// and reads the `X-Sqd-Finalized-Head-Number` response header (the portal
    /// returns it regardless of whether the range yields data).
    pub(crate) async fn finalized_head(&self) -> Result<BlockNumber> {
        let body = json!({ "type": "substrate", "fromBlock": 0, "toBlock": 0 });
        let resp = self
            .http
            .post(&self.stream_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SubdexError::Source(format!("portal head request: {e}")))?;
        let resp = error_for_status(resp)?;
        parse_head_header(&resp)
    }

    /// Fetch decoded blocks in `[from, to]` (inclusive), requesting only the
    /// fields `selection` needs. Returns the parsed blocks in ascending height.
    pub(crate) async fn fetch_range(
        &self,
        from: BlockNumber,
        to: BlockNumber,
        selection: DataSelection,
    ) -> Result<Vec<PortalBlock>> {
        let body = self.stream_request(from, to, selection);
        let resp = self
            .http
            .post(&self.stream_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| SubdexError::Source(format!("portal stream request: {e}")))?;
        let resp = error_for_status(resp)?;
        let text = resp
            .text()
            .await
            .map_err(|e| SubdexError::Source(format!("portal stream body: {e}")))?;
        parse_json_lines(&text)
    }

    /// Build the `/stream` request body for a range + selection. The `fields`
    /// selector asks the portal for exactly the header/event/call fields we map,
    /// and `includeAllBlocks` ensures every block in range is returned (not just
    /// those matching a filter) so heights stay contiguous for reorg checks.
    fn stream_request(
        &self,
        from: BlockNumber,
        to: BlockNumber,
        selection: DataSelection,
    ) -> serde_json::Value {
        let mut fields = json!({
            "block": {
                "number": true, "hash": true, "parentHash": true,
                "specVersion": true, "timestamp": true
            }
        });
        if selection.events {
            fields["event"] = json!({
                "name": true, "args": true, "phase": true, "extrinsicIndex": true
            });
        }
        if selection.extrinsics {
            fields["call"] = json!({
                "name": true, "args": true, "success": true, "origin": true
            });
        }

        let mut req = json!({
            "type": "substrate",
            "fromBlock": from,
            "toBlock": to,
            "includeAllBlocks": true,
            "fields": fields,
        });
        // Ask for all events / all calls (empty filter object = match everything),
        // but only for the parts we selected.
        if selection.events {
            req["events"] = json!([{}]);
        }
        if selection.extrinsics {
            req["calls"] = json!([{}]);
        }
        req
    }
}

/// Turn a non-2xx portal response into a `Source` error (retryable).
fn error_for_status(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else {
        Err(SubdexError::Source(format!(
            "portal returned HTTP {status}"
        )))
    }
}

/// Read the finalized-head height from the response header.
fn parse_head_header(resp: &reqwest::Response) -> Result<BlockNumber> {
    let raw = resp.headers().get(FINALIZED_HEAD_HEADER).ok_or_else(|| {
        SubdexError::Source(format!(
            "portal response missing {FINALIZED_HEAD_HEADER} header"
        ))
    })?;
    let s = raw
        .to_str()
        .map_err(|e| SubdexError::Source(format!("bad finalized-head header: {e}")))?;
    s.trim().parse::<BlockNumber>().map_err(|e| {
        SubdexError::Source(format!("finalized-head header not a number ({s:?}): {e}"))
    })
}

/// Parse a JSON-lines body: one [`PortalBlock`] per non-empty line, in order.
/// Blank lines are skipped; a malformed line is a `Decode` error (not retryable).
fn parse_json_lines(text: &str) -> Result<Vec<PortalBlock>> {
    let mut blocks = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let pb: PortalBlock = serde_json::from_str(line)
            .map_err(|e| SubdexError::Decode(format!("portal block line {i}: {e}")))?;
        blocks.push(pb);
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_lines_skipping_blanks() {
        let body = "\
{\"header\":{\"number\":1,\"hash\":\"0x1\",\"parentHash\":\"0x0\"}}

{\"header\":{\"number\":2,\"hash\":\"0x2\",\"parentHash\":\"0x1\"}}
";
        let blocks = parse_json_lines(body).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].header.number, 1);
        assert_eq!(blocks[1].header.number, 2);
    }

    #[test]
    fn malformed_line_is_a_decode_error() {
        let err = parse_json_lines("{not json}").unwrap_err();
        assert!(matches!(err, SubdexError::Decode(_)));
    }

    #[test]
    fn empty_body_yields_no_blocks() {
        assert!(parse_json_lines("").unwrap().is_empty());
        assert!(parse_json_lines("\n\n").unwrap().is_empty());
    }

    #[test]
    fn stream_request_honours_selection() {
        let c = PortalClient::new("https://portal.sqd.dev", "polkadot").unwrap();
        // events only
        let req = c.stream_request(10, 20, DataSelection::events_only());
        assert_eq!(req["fromBlock"], 10);
        assert_eq!(req["toBlock"], 20);
        assert_eq!(req["includeAllBlocks"], true);
        assert!(req["fields"]["event"].is_object());
        assert!(
            req["fields"]["call"].is_null(),
            "no call fields when extrinsics off"
        );
        assert!(req["events"].is_array());
        assert!(req["calls"].is_null());

        // extrinsics only
        let req = c.stream_request(0, 5, DataSelection::extrinsics_only());
        assert!(req["fields"]["call"].is_object());
        assert!(req["fields"]["event"].is_null());
        assert!(req["calls"].is_array());
        assert!(req["events"].is_null());
    }

    #[test]
    fn builds_stream_url() {
        let c = PortalClient::new("https://portal.sqd.dev/", "kusama").unwrap();
        assert_eq!(
            c.stream_url,
            "https://portal.sqd.dev/datasets/kusama/stream"
        );
    }
}
