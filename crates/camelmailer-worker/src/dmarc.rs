//! DMARC aggregate-report (RUA) parsing: pull the report XML out of an
//! inbound message (attachments as `.xml`, `.xml.gz` or `.zip`, or XML
//! directly in the body) and parse it per RFC 7489 Appendix C.
//!
//! Everything here is infallible from the worker's point of view: any
//! malformed input becomes a [`DmarcParseError`] the caller turns into a
//! held message — never a crash.

use camelmailer_core::message::parse_headers;
use chrono::{DateTime, Utc};
use std::io::Read;

/// Hard cap for a decompressed report (zip-bomb protection).
const MAX_REPORT_BYTES: u64 = 20 * 1024 * 1024;
/// MIME parts can nest; reports never need more than this.
const MAX_MIME_DEPTH: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum DmarcParseError {
    #[error("the message carries no parseable DMARC aggregate report: {0}")]
    NoReport(String),
    #[error("invalid report XML: {0}")]
    Xml(String),
    #[error("the report is missing required field {0}")]
    MissingField(&'static str),
    #[error("could not decompress the report: {0}")]
    Decompress(String),
}

/// A parsed aggregate report, independent of any tenant.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateReport {
    pub org_name: Option<String>,
    pub org_email: Option<String>,
    pub report_id: String,
    /// `policy_published/domain` — the domain the report is about.
    pub domain: String,
    pub date_range_begin: DateTime<Utc>,
    pub date_range_end: DateTime<Utc>,
    pub records: Vec<AggregateRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateRecord {
    pub source_ip: String,
    pub count: i64,
    pub disposition: String,
    pub dkim_result: Option<String>,
    pub spf_result: Option<String>,
    pub dkim_aligned: bool,
    pub spf_aligned: bool,
    pub header_from: Option<String>,
    pub envelope_from: Option<String>,
}

// ------------------------------------------------------------- XML parsing

fn epoch_to_datetime(value: &str, field: &'static str) -> Result<DateTime<Utc>, DmarcParseError> {
    let seconds: i64 = value
        .trim()
        .parse()
        .map_err(|_| DmarcParseError::MissingField(field))?;
    DateTime::<Utc>::from_timestamp(seconds, 0).ok_or(DmarcParseError::MissingField(field))
}

/// Parse aggregate-report XML (RFC 7489 Appendix C). Tolerant of extra
/// elements and ordering; strict about the fields the storage needs.
pub fn parse_report_xml(xml: &[u8]) -> Result<AggregateReport, DmarcParseError> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    // element path, e.g. ["feedback", "record", "row", "source_ip"]
    let mut path: Vec<String> = Vec::new();
    let mut buffer = Vec::new();

    let mut org_name = None;
    let mut org_email = None;
    let mut report_id = None;
    let mut domain = None;
    let mut begin = None;
    let mut end = None;
    let mut records: Vec<AggregateRecord> = Vec::new();
    let mut current: Option<AggregateRecord> = None;

    fn blank_record() -> AggregateRecord {
        AggregateRecord {
            source_ip: String::new(),
            count: 0,
            disposition: "none".into(),
            dkim_result: None,
            spf_result: None,
            dkim_aligned: false,
            spf_aligned: false,
            header_from: None,
            envelope_from: None,
        }
    }

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                let name = String::from_utf8_lossy(start.name().as_ref()).to_lowercase();
                if path.is_empty() && name != "feedback" {
                    return Err(DmarcParseError::Xml(format!(
                        "unexpected root element <{name}> (expected <feedback>)"
                    )));
                }
                if path.len() == 1 && name == "record" {
                    current = Some(blank_record());
                }
                path.push(name);
            }
            Ok(Event::End(_)) => {
                if path.len() == 2 && path[1] == "record" {
                    if let Some(record) = current.take() {
                        records.push(record);
                    }
                }
                path.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text
                    .unescape()
                    .map_err(|error| DmarcParseError::Xml(error.to_string()))?
                    .trim()
                    .to_string();
                if value.is_empty() {
                    continue;
                }
                let joined = path.join("/");
                match joined.as_str() {
                    "feedback/report_metadata/org_name" => org_name = Some(value),
                    "feedback/report_metadata/email" => org_email = Some(value),
                    "feedback/report_metadata/report_id" => report_id = Some(value),
                    "feedback/report_metadata/date_range/begin" => {
                        begin = Some(epoch_to_datetime(&value, "date_range/begin")?)
                    }
                    "feedback/report_metadata/date_range/end" => {
                        end = Some(epoch_to_datetime(&value, "date_range/end")?)
                    }
                    "feedback/policy_published/domain" => domain = Some(value),
                    "feedback/record/row/source_ip" => {
                        if let Some(record) = current.as_mut() {
                            record.source_ip = value;
                        }
                    }
                    "feedback/record/row/count" => {
                        if let Some(record) = current.as_mut() {
                            record.count = value.parse().unwrap_or(0);
                        }
                    }
                    "feedback/record/row/policy_evaluated/disposition" => {
                        if let Some(record) = current.as_mut() {
                            record.disposition = value.to_lowercase();
                        }
                    }
                    "feedback/record/row/policy_evaluated/dkim" => {
                        if let Some(record) = current.as_mut() {
                            record.dkim_aligned = value.eq_ignore_ascii_case("pass");
                        }
                    }
                    "feedback/record/row/policy_evaluated/spf" => {
                        if let Some(record) = current.as_mut() {
                            record.spf_aligned = value.eq_ignore_ascii_case("pass");
                        }
                    }
                    "feedback/record/identifiers/header_from" => {
                        if let Some(record) = current.as_mut() {
                            record.header_from = Some(value);
                        }
                    }
                    "feedback/record/identifiers/envelope_from" => {
                        if let Some(record) = current.as_mut() {
                            record.envelope_from = Some(value);
                        }
                    }
                    // first auth result wins (reports may list several)
                    "feedback/record/auth_results/dkim/result" => {
                        if let Some(record) = current.as_mut() {
                            record
                                .dkim_result
                                .get_or_insert_with(|| value.to_lowercase());
                        }
                    }
                    "feedback/record/auth_results/spf/result" => {
                        if let Some(record) = current.as_mut() {
                            record
                                .spf_result
                                .get_or_insert_with(|| value.to_lowercase());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => {
                if !path.is_empty() {
                    return Err(DmarcParseError::Xml(format!(
                        "unexpected end of file inside <{}>",
                        path.join("/")
                    )));
                }
                break;
            }
            Ok(_) => {}
            Err(error) => return Err(DmarcParseError::Xml(error.to_string())),
        }
        buffer.clear();
    }

    Ok(AggregateReport {
        org_name,
        org_email,
        report_id: report_id.ok_or(DmarcParseError::MissingField("report_id"))?,
        domain: domain.ok_or(DmarcParseError::MissingField("policy_published/domain"))?,
        date_range_begin: begin.ok_or(DmarcParseError::MissingField("date_range/begin"))?,
        date_range_end: end.ok_or(DmarcParseError::MissingField("date_range/end"))?,
        records,
    })
}

// -------------------------------------------------------------- containers

/// Turn a candidate payload into report XML by content sniffing:
/// gzip (`1f 8b`) → gunzip, zip (`PK..`) → first `.xml` entry, anything
/// else is assumed to be XML already.
fn payload_to_xml(bytes: &[u8]) -> Result<Vec<u8>, DmarcParseError> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut xml = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .take(MAX_REPORT_BYTES)
            .read_to_end(&mut xml)
            .map_err(|error| DmarcParseError::Decompress(error.to_string()))?;
        return Ok(xml);
    }
    if bytes.starts_with(b"PK") {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|error| DmarcParseError::Decompress(error.to_string()))?;
        // prefer the first *.xml entry; fall back to the first file
        let index = (0..archive.len())
            .find(|&i| {
                archive
                    .by_index(i)
                    .map(|f| f.name().to_lowercase().ends_with(".xml"))
                    .unwrap_or(false)
            })
            .unwrap_or(0);
        let mut file = archive
            .by_index(index)
            .map_err(|error| DmarcParseError::Decompress(error.to_string()))?;
        let mut xml = Vec::new();
        std::io::Read::take(&mut file as &mut dyn Read, MAX_REPORT_BYTES)
            .read_to_end(&mut xml)
            .map_err(|error| DmarcParseError::Decompress(error.to_string()))?;
        return Ok(xml);
    }
    Ok(bytes.to_vec())
}

// -------------------------------------------------------------------- MIME

/// Decode a quoted-printable body (soft breaks and =XX escapes).
fn decode_quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'=' {
            // soft line break: =\r\n or =\n
            if body.get(i + 1) == Some(&b'\r') && body.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if body.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if i + 2 < body.len() {
                let hex = std::str::from_utf8(&body[i + 1..i + 3]).ok();
                if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(body[i]);
        i += 1;
    }
    out
}

/// The body bytes of a raw MIME entity (everything after the first blank
/// line).
fn body_of(raw: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < raw.len() {
        let line_end = raw[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| i + p)
            .unwrap_or(raw.len());
        let line = &raw[i..line_end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return &raw[(line_end + 1).min(raw.len())..];
        }
        i = line_end + 1;
    }
    &[]
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The boundary parameter of a multipart content-type, if any.
fn multipart_boundary(content_type: &str) -> Option<String> {
    if !content_type.trim().to_lowercase().starts_with("multipart/") {
        return None;
    }
    for parameter in content_type.split(';').skip(1) {
        let parameter = parameter.trim();
        if let Some(value) = parameter
            .strip_prefix("boundary=")
            .or_else(|| parameter.strip_prefix("BOUNDARY="))
            .or_else(|| parameter.strip_prefix("Boundary="))
        {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// Collect the decoded payloads of all leaf MIME parts (depth-first).
fn collect_leaf_payloads(raw: &[u8], depth: usize, payloads: &mut Vec<Vec<u8>>) {
    if depth > MAX_MIME_DEPTH {
        return;
    }
    let headers = parse_headers(raw);
    let body = body_of(raw);

    if let Some(boundary) = header(&headers, "content-type").and_then(multipart_boundary) {
        let delimiter = format!("--{boundary}");
        let text = String::from_utf8_lossy(body);
        let mut parts: Vec<&str> = text.split(&delimiter).collect();
        // parts[0] is the preamble; the last piece after `--<boundary>--`
        // is the epilogue
        if parts.len() > 1 {
            parts.remove(0);
        }
        for part in parts {
            let part = part.strip_prefix("--").unwrap_or(part); // closing marker
            let part = part.trim_start_matches(['\r', '\n']);
            if part.trim().is_empty() {
                continue;
            }
            collect_leaf_payloads(part.as_bytes(), depth + 1, payloads);
        }
        return;
    }

    let encoding = header(&headers, "content-transfer-encoding")
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let decoded = match encoding.as_str() {
        "base64" => {
            use base64::Engine;
            let compact: Vec<u8> = body
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(&compact)
                .unwrap_or_else(|_| body.to_vec())
        }
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    };
    if !decoded.is_empty() {
        payloads.push(decoded);
    }
}

/// Extract and parse the DMARC aggregate report carried by a raw inbound
/// message: every MIME leaf part (and the plain body) is tried as
/// `.xml` / `.xml.gz` / `.zip` payload; the first part that parses as a
/// report wins.
pub fn extract_report(raw_message: &[u8]) -> Result<AggregateReport, DmarcParseError> {
    let mut payloads = Vec::new();
    collect_leaf_payloads(raw_message, 0, &mut payloads);
    if payloads.is_empty() {
        // no header block at all? try the raw bytes as a last resort
        payloads.push(raw_message.to_vec());
    }

    let mut last_error: Option<DmarcParseError> = None;
    for payload in &payloads {
        let xml = match payload_to_xml(payload) {
            Ok(xml) => xml,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        // cheap pre-filter: only feed things that can be a report into
        // the XML parser, so a text/plain cover note is skipped silently
        let looks_like_xml = xml
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map(|i| xml[i] == b'<')
            .unwrap_or(false);
        if !looks_like_xml {
            continue;
        }
        match parse_report_xml(&xml) {
            Ok(report) => return Ok(report),
            Err(error) => last_error = Some(error),
        }
    }
    Err(DmarcParseError::NoReport(
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no XML, gzip or zip payload found".into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/dmarc/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn zip_with(name: &str, bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file::<_, ()>(name, zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn parses_the_rfc_7489_appendix_c_style_report() {
        let report = parse_report_xml(&fixture("rfc-appendix-c.xml")).unwrap();
        assert_eq!(report.org_name.as_deref(), Some("acme.com"));
        assert_eq!(
            report.org_email.as_deref(),
            Some("noreply-dmarc-support@acme.com")
        );
        assert_eq!(report.report_id, "9391651994964116463");
        assert_eq!(report.domain, "example.com");
        assert_eq!(report.date_range_begin.timestamp(), 1335571200);
        assert_eq!(report.date_range_end.timestamp(), 1335657599);
        assert_eq!(report.records.len(), 1);
        let record = &report.records[0];
        assert_eq!(record.source_ip, "72.150.241.94");
        assert_eq!(record.count, 2);
        assert_eq!(record.disposition, "none");
        assert!(!record.dkim_aligned); // policy_evaluated dkim=fail
        assert!(record.spf_aligned);
        assert_eq!(record.dkim_result.as_deref(), Some("fail"));
        assert_eq!(record.spf_result.as_deref(), Some("pass"));
        assert_eq!(record.header_from.as_deref(), Some("example.com"));
        assert_eq!(record.envelope_from.as_deref(), Some("example.com"));
    }

    #[test]
    fn parses_a_google_style_report_with_multiple_records() {
        let report = parse_report_xml(&fixture("google-style.xml")).unwrap();
        assert_eq!(report.org_name.as_deref(), Some("google.com"));
        assert_eq!(report.records.len(), 2);
        assert!(report.records[0].dkim_aligned && report.records[0].spf_aligned);
        assert_eq!(report.records[1].disposition, "quarantine");
        assert!(!report.records[1].spf_aligned);
    }

    #[test]
    fn parses_a_microsoft_style_report() {
        let report = parse_report_xml(&fixture("microsoft-style.xml")).unwrap();
        assert_eq!(
            report.org_name.as_deref(),
            Some("Outlook.com aggregate example")
        );
        assert_eq!(report.domain, "example.com");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].count, 14);
    }

    #[test]
    fn broken_inputs_error_instead_of_panicking() {
        // truncated XML
        let full = fixture("rfc-appendix-c.xml");
        assert!(parse_report_xml(&full[..full.len() / 2]).is_err());
        // not XML at all
        assert!(parse_report_xml(b"this is not xml").is_err());
        // wrong root element
        assert!(parse_report_xml(b"<report><x/></report>").is_err());
        // missing required fields
        assert!(matches!(
            parse_report_xml(
                b"<feedback><report_metadata><report_id>1</report_id></report_metadata></feedback>"
            ),
            Err(DmarcParseError::MissingField(_))
        ));
        // corrupt gzip payload inside a message
        let mut broken_gz = gzip(&full);
        let length = broken_gz.len();
        broken_gz.truncate(length / 2);
        assert!(
            payload_to_xml(&broken_gz).is_err()
                || parse_report_xml(&payload_to_xml(&broken_gz).unwrap()).is_err()
        );
    }

    #[test]
    fn gzip_and_zip_containers_are_unpacked() {
        let xml = fixture("rfc-appendix-c.xml");
        assert_eq!(payload_to_xml(&gzip(&xml)).unwrap(), xml);
        assert_eq!(
            payload_to_xml(&zip_with("acme.com!example.com!1.xml", &xml)).unwrap(),
            xml
        );
        // zip: the first .xml entry wins even after other entries
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        use std::io::Write;
        writer
            .start_file::<_, ()>("readme.txt", zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(b"cover note").unwrap();
        writer
            .start_file::<_, ()>("report.xml", zip::write::FileOptions::default())
            .unwrap();
        writer.write_all(&xml).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(payload_to_xml(&bytes).unwrap(), xml);
    }

    fn base64_lines(bytes: &[u8]) -> String {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        encoded
            .as_bytes()
            .chunks(76)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    fn multipart_message(filename: &str, content_type: &str, payload: &[u8]) -> Vec<u8> {
        format!(
            "From: reporter@acme.com\r\n\
             To: dmarc@example.com\r\n\
             Subject: Report Domain: example.com\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"XYZ\"\r\n\
             \r\n\
             --XYZ\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             This is an aggregate report.\r\n\
             --XYZ\r\n\
             Content-Type: {content_type}; name=\"{filename}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\
             Content-Disposition: attachment; filename=\"{filename}\"\r\n\
             \r\n\
             {}\r\n\
             --XYZ--\r\n",
            base64_lines(payload)
        )
        .into_bytes()
    }

    #[test]
    fn extracts_the_report_from_a_gzip_attachment() {
        let xml = fixture("rfc-appendix-c.xml");
        let message = multipart_message(
            "acme.com!example.com!1335571200!1335657599.xml.gz",
            "application/gzip",
            &gzip(&xml),
        );
        let report = extract_report(&message).unwrap();
        assert_eq!(report.domain, "example.com");
        assert_eq!(report.records.len(), 1);
    }

    #[test]
    fn extracts_the_report_from_a_zip_attachment() {
        let xml = fixture("google-style.xml");
        let message = multipart_message(
            "google.com!example.com!1.zip",
            "application/zip",
            &zip_with("google.com!example.com!1.xml", &xml),
        );
        let report = extract_report(&message).unwrap();
        assert_eq!(report.org_name.as_deref(), Some("google.com"));
    }

    #[test]
    fn extracts_the_report_from_a_plain_xml_attachment_and_body() {
        let xml = fixture("microsoft-style.xml");
        let message = multipart_message("report.xml", "text/xml", &xml);
        assert_eq!(extract_report(&message).unwrap().records.len(), 1);

        // XML directly in a non-multipart body
        let direct = [
            b"From: reporter@acme.com\r\nContent-Type: text/xml\r\n\r\n".to_vec(),
            xml.clone(),
        ]
        .concat();
        assert_eq!(extract_report(&direct).unwrap().domain, "example.com");
    }

    #[test]
    fn messages_without_a_report_error_gracefully() {
        let message = b"From: someone@example.com\r\n\
            Content-Type: text/plain\r\n\r\nJust a normal mail.\r\n";
        assert!(matches!(
            extract_report(message),
            Err(DmarcParseError::NoReport(_))
        ));

        // an attachment that decompresses to garbage
        let message = multipart_message("x.xml.gz", "application/gzip", &gzip(b"not xml"));
        assert!(extract_report(&message).is_err());
    }
}
