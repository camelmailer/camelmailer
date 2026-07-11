//! Minimal, strict XML-DSig verification for the SAML service provider.
//!
//! Scope is deliberately narrow — exactly what a SAML SP needs and
//! nothing more:
//!
//! - enveloped signatures (`ds:Signature` as a direct child of the
//!   signed element) with a single same-document `Reference`
//! - exclusive canonicalization (`xml-exc-c14n#`, with
//!   `InclusiveNamespaces PrefixList` support)
//! - `rsa-sha256` signatures and `sha256` digests only — `rsa-sha1` and
//!   friends are rejected
//! - the key comes exclusively from the configured IdP certificate;
//!   `ds:KeyInfo` in the document is ignored
//!
//! Signature-wrapping defences: the `Reference` must point at the very
//! element the caller wants verified, and the referenced `ID` value must
//! be unique in the whole document.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use roxmltree::{Document, Node, NodeId};
use rsa::pkcs8::DecodePublicKey;
use rsa::RsaPublicKey;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const NS_DSIG: &str = "http://www.w3.org/2000/09/xmldsig#";
const ALG_EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
const ALG_RSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";
const ALG_SHA256: &str = "http://www.w3.org/2001/04/xmlenc#sha256";
const ALG_ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";

/// Decode base64 that may carry whitespace/newlines (as SAML tends to).
pub fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(compact.as_bytes())
        .map_err(|error| format!("invalid base64: {error}"))
}

/// Extract the RSA public key from an X.509 certificate in PEM form.
pub fn public_key_from_certificate_pem(pem: &str) -> Result<RsaPublicKey, String> {
    let der = pem_block(pem, "CERTIFICATE")?;
    let certificate = <x509_cert::Certificate as x509_cert::der::Decode>::from_der(&der)
        .map_err(|error| format!("invalid X.509 certificate: {error}"))?;
    let spki = x509_cert::der::Encode::to_der(&certificate.tbs_certificate.subject_public_key_info)
        .map_err(|error| format!("invalid certificate key: {error}"))?;
    RsaPublicKey::from_public_key_der(&spki)
        .map_err(|error| format!("the certificate does not carry an RSA key: {error}"))
}

fn pem_block(pem: &str, label: &str) -> Result<Vec<u8>, String> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = pem
        .find(&begin)
        .ok_or_else(|| format!("no {begin} block found"))?
        + begin.len();
    let stop = pem[start..]
        .find(&end)
        .ok_or_else(|| format!("no {end} marker found"))?;
    decode_base64(&pem[start..start + stop])
}

/// The `ds:Signature` that is a *direct* child of `element`, if any.
pub fn direct_signature<'a, 'input>(element: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    element.children().find(|child| {
        child.is_element()
            && child.tag_name().namespace() == Some(NS_DSIG)
            && child.tag_name().name() == "Signature"
    })
}

fn ds_child<'a, 'input>(parent: Node<'a, 'input>, name: &str) -> Result<Node<'a, 'input>, String> {
    parent
        .children()
        .find(|child| {
            child.is_element()
                && child.tag_name().namespace() == Some(NS_DSIG)
                && child.tag_name().name() == name
        })
        .ok_or_else(|| format!("the signature is missing its {name} element"))
}

fn inclusive_prefixes(node: Node) -> Vec<String> {
    // <ec:InclusiveNamespaces PrefixList="a b c"/> inside a Transform or
    // CanonicalizationMethod.
    node.children()
        .filter(|child| child.is_element() && child.tag_name().name() == "InclusiveNamespaces")
        .filter_map(|child| child.attribute("PrefixList"))
        .flat_map(|list| list.split_whitespace().map(str::to_string))
        .collect()
}

/// Verify the enveloped signature on `element` against `public_key`.
///
/// Checks, in order: the signature algorithm set (exclusive C14N +
/// RSA-SHA256 + SHA-256 only), that the single `Reference` points at
/// `element` via a document-unique `ID`, the content digest, and finally
/// the RSA signature over the canonicalized `SignedInfo`.
pub fn verify_enveloped_signature(
    document: &Document,
    element: Node,
    public_key: &RsaPublicKey,
) -> Result<(), String> {
    let signature =
        direct_signature(element).ok_or("the element does not carry an XML signature")?;
    let signed_info = ds_child(signature, "SignedInfo")?;

    let c14n = ds_child(signed_info, "CanonicalizationMethod")?;
    if c14n.attribute("Algorithm") != Some(ALG_EXC_C14N) {
        return Err(format!(
            "unsupported canonicalization {:?} (only exclusive C14N is accepted)",
            c14n.attribute("Algorithm").unwrap_or("")
        ));
    }
    let signed_info_prefixes = inclusive_prefixes(c14n);

    let method = ds_child(signed_info, "SignatureMethod")?;
    if method.attribute("Algorithm") != Some(ALG_RSA_SHA256) {
        return Err(format!(
            "unsupported signature algorithm {:?} (only rsa-sha256 is accepted)",
            method.attribute("Algorithm").unwrap_or("")
        ));
    }

    let references: Vec<Node> = signed_info
        .children()
        .filter(|child| {
            child.is_element()
                && child.tag_name().namespace() == Some(NS_DSIG)
                && child.tag_name().name() == "Reference"
        })
        .collect();
    let [reference] = references[..] else {
        return Err("the signature must contain exactly one Reference".into());
    };
    let uri = reference.attribute("URI").unwrap_or("");
    let id = uri
        .strip_prefix('#')
        .ok_or("only same-document references are supported")?;
    if element.attribute("ID") != Some(id) {
        return Err("the signature does not cover this element".into());
    }
    // Signature-wrapping defence: the referenced ID must identify exactly
    // one element in the whole document.
    let occurrences = document
        .descendants()
        .filter(|node| node.is_element() && node.attribute("ID") == Some(id))
        .count();
    if occurrences != 1 {
        return Err("the referenced ID is not unique within the document".into());
    }

    let transforms = ds_child(reference, "Transforms")?;
    let mut saw_enveloped = false;
    let mut reference_prefixes: Vec<String> = vec![];
    for transform in transforms
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == "Transform")
    {
        match transform.attribute("Algorithm").unwrap_or("") {
            ALG_ENVELOPED => saw_enveloped = true,
            ALG_EXC_C14N => reference_prefixes.extend(inclusive_prefixes(transform)),
            other => return Err(format!("unsupported transform {other:?}")),
        }
    }
    if !saw_enveloped {
        return Err("the reference must use the enveloped-signature transform".into());
    }

    let digest_method = ds_child(reference, "DigestMethod")?;
    if digest_method.attribute("Algorithm") != Some(ALG_SHA256) {
        return Err(format!(
            "unsupported digest algorithm {:?} (only sha256 is accepted)",
            digest_method.attribute("Algorithm").unwrap_or("")
        ));
    }
    let digest_value = ds_child(reference, "DigestValue")?
        .text()
        .map(decode_base64)
        .transpose()?
        .unwrap_or_default();

    let canonical = canonicalize(element, Some(signature.id()), &reference_prefixes);
    if Sha256::digest(canonical.as_bytes()).as_slice() != digest_value.as_slice() {
        return Err("digest mismatch: the signed content was modified".into());
    }

    let signature_value = ds_child(signature, "SignatureValue")?
        .text()
        .map(decode_base64)
        .transpose()?
        .unwrap_or_default();
    let signed_info_canonical = canonicalize(signed_info, None, &signed_info_prefixes);

    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    let verifying_key = VerifyingKey::<Sha256>::new(public_key.clone());
    let signature = Signature::try_from(signature_value.as_slice())
        .map_err(|error| format!("malformed signature value: {error}"))?;
    verifying_key
        .verify(signed_info_canonical.as_bytes(), &signature)
        .map_err(|_| "signature verification failed".to_string())
}

// -------------------------------------------------------- canonicalization

/// Serialize an element subtree with exclusive XML canonicalization
/// (http://www.w3.org/2001/10/xml-exc-c14n#, without comments).
/// `exclude` removes one subtree (the enveloped `ds:Signature`);
/// `inclusive` is the `InclusiveNamespaces PrefixList`.
pub fn canonicalize(element: Node, exclude: Option<NodeId>, inclusive: &[String]) -> String {
    let mut out = String::new();
    write_node(&mut out, element, exclude, inclusive, &BTreeMap::new());
    out
}

type RenderedNs = BTreeMap<Option<String>, String>;

fn write_node(
    out: &mut String,
    node: Node,
    exclude: Option<NodeId>,
    inclusive: &[String],
    rendered: &RenderedNs,
) {
    if Some(node.id()) == exclude {
        return;
    }
    if node.is_text() {
        escape_text(out, node.text().unwrap_or(""));
        return;
    }
    if node.is_pi() {
        let pi = node.pi().expect("pi node");
        out.push_str("<?");
        out.push_str(pi.target);
        if let Some(value) = pi.value {
            out.push(' ');
            out.push_str(value);
        }
        out.push_str("?>");
        return;
    }
    if !node.is_element() {
        return; // comments are dropped
    }

    let qname = element_qname(node);
    let prefix = qname.split(':').next().filter(|_| qname.contains(':'));
    out.push('<');
    out.push_str(qname);

    // --- namespace declarations (exclusive: only visibly utilized ones)
    let mut scope = rendered.clone();
    let mut declarations: Vec<(String, String)> = vec![]; // (prefix or "", uri)
    let consider =
        |scope: &mut RenderedNs, declarations: &mut Vec<(String, String)>, prefix: Option<&str>| {
            if prefix == Some("xml") || prefix == Some("xmlns") {
                return;
            }
            let uri = node
                .lookup_namespace_uri(prefix)
                .map(str::to_string)
                .unwrap_or_default();
            if prefix.is_none() && uri.is_empty() {
                // Element in no namespace: undeclare an inherited default.
                if !scope
                    .get(&None)
                    .map(String::as_str)
                    .unwrap_or("")
                    .is_empty()
                {
                    scope.insert(None, String::new());
                    declarations.push((String::new(), String::new()));
                }
                return;
            }
            if uri.is_empty() {
                return; // prefix not in scope (cannot happen in parsed XML)
            }
            let key = prefix.map(str::to_string);
            if scope.get(&key).map(String::as_str) != Some(uri.as_str()) {
                scope.insert(key, uri.clone());
                declarations.push((prefix.unwrap_or("").to_string(), uri));
            }
        };

    consider(&mut scope, &mut declarations, prefix);
    for attribute in node.attributes() {
        if let Some(namespace) = attribute.namespace() {
            if let Some(attr_prefix) = node.lookup_prefix(namespace) {
                consider(&mut scope, &mut declarations, Some(attr_prefix));
            }
        }
    }
    for prefix in inclusive {
        consider(&mut scope, &mut declarations, Some(prefix.as_str()));
    }
    declarations.sort();
    for (declared_prefix, uri) in &declarations {
        if declared_prefix.is_empty() {
            out.push_str(" xmlns=\"");
        } else {
            out.push_str(" xmlns:");
            out.push_str(declared_prefix);
            out.push_str("=\"");
        }
        escape_attribute(out, uri);
        out.push('"');
    }

    // --- attributes, sorted by (namespace URI, local name)
    let mut attributes: Vec<(&str, &str, String, &str)> = node
        .attributes()
        .map(|attribute| {
            let namespace = attribute.namespace().unwrap_or("");
            let qname = match attribute.namespace() {
                Some("http://www.w3.org/XML/1998/namespace") => {
                    format!("xml:{}", attribute.name())
                }
                Some(uri) => match node.lookup_prefix(uri) {
                    Some(attr_prefix) => format!("{attr_prefix}:{}", attribute.name()),
                    None => attribute.name().to_string(),
                },
                None => attribute.name().to_string(),
            };
            (namespace, attribute.name(), qname, attribute.value())
        })
        .collect();
    attributes.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    for (_, _, qname, value) in &attributes {
        out.push(' ');
        out.push_str(qname);
        out.push_str("=\"");
        escape_attribute(out, value);
        out.push('"');
    }

    out.push('>');
    for child in node.children() {
        write_node(out, child, exclude, inclusive, &scope);
    }
    out.push_str("</");
    out.push_str(qname);
    out.push('>');
}

/// The element's qualified name exactly as written in the source
/// (canonicalization must preserve original prefixes).
fn element_qname<'input>(node: Node<'_, 'input>) -> &'input str {
    let input = node.document().input_text();
    let raw = &input[node.range()];
    let name = &raw[1..];
    let end = name
        .find([' ', '\t', '\n', '\r', '>', '/'])
        .unwrap_or(name.len());
    &name[..end]
}

fn escape_text(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            other => out.push(other),
        }
    }
}

fn escape_attribute(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::RsaPrivateKey;
    use std::sync::OnceLock;

    fn test_key() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap())
    }

    fn c14n_str(xml: &str) -> String {
        let doc = Document::parse(xml).unwrap();
        canonicalize(doc.root_element(), None, &[])
    }

    #[test]
    fn c14n_expands_self_closing_and_sorts_attributes() {
        assert_eq!(c14n_str("<a/>"), "<a></a>");
        assert_eq!(
            c14n_str(r#"<a z="1" b="2" a="3"/>"#),
            r#"<a a="3" b="2" z="1"></a>"#
        );
    }

    #[test]
    fn c14n_renders_only_used_namespaces_once() {
        let xml = r#"<p:root xmlns:p="urn:p" xmlns:unused="urn:u"><p:child><p:leaf>x</p:leaf></p:child></p:root>"#;
        assert_eq!(
            c14n_str(xml),
            r#"<p:root xmlns:p="urn:p"><p:child><p:leaf>x</p:leaf></p:child></p:root>"#
        );
    }

    #[test]
    fn c14n_renders_namespaces_where_first_visible() {
        // saml declared on the document root but first used on the child:
        // exclusive C14N of the child renders it there.
        let xml = r#"<r xmlns:s="urn:s"><s:a t="1">v</s:a></r>"#;
        let doc = Document::parse(xml).unwrap();
        let child = doc
            .root_element()
            .children()
            .find(|n| n.is_element())
            .unwrap();
        assert_eq!(
            canonicalize(child, None, &[]),
            r#"<s:a xmlns:s="urn:s" t="1">v</s:a>"#
        );
    }

    #[test]
    fn c14n_escapes_text_and_attributes() {
        assert_eq!(
            c14n_str("<a b=\"x&amp;y\">1 &lt; 2 &amp; 3 &gt; 2</a>"),
            "<a b=\"x&amp;y\">1 &lt; 2 &amp; 3 &gt; 2</a>"
        );
    }

    #[test]
    fn c14n_handles_default_namespaces() {
        let xml = r#"<a xmlns="urn:d"><b>x</b></a>"#;
        assert_eq!(c14n_str(xml), r#"<a xmlns="urn:d"><b>x</b></a>"#);
    }

    #[test]
    fn c14n_excludes_a_subtree() {
        let xml = r#"<a ID="x"><keep>1</keep><drop>2</drop></a>"#;
        let doc = Document::parse(xml).unwrap();
        let drop = doc
            .descendants()
            .find(|n| n.has_tag_name("drop"))
            .unwrap()
            .id();
        assert_eq!(
            canonicalize(doc.root_element(), Some(drop), &[]),
            r#"<a ID="x"><keep>1</keep></a>"#
        );
    }

    /// Build a signed element the way the SAML tests' mock IdP does and
    /// check that verification round-trips — and fails closed on
    /// tampering.
    fn signed_document(tamper: bool) -> String {
        let body = r#"<t:Doc xmlns:t="urn:test" ID="_doc1"><t:Issuer>idp</t:Issuer><t:Value>payload</t:Value></t:Doc>"#;
        let digest = STANDARD.encode(Sha256::digest(body.as_bytes()));
        let signed_info = format!(
            "<ds:SignedInfo xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\">\
             <ds:CanonicalizationMethod Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></ds:CanonicalizationMethod>\
             <ds:SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha256\"></ds:SignatureMethod>\
             <ds:Reference URI=\"#_doc1\">\
             <ds:Transforms>\
             <ds:Transform Algorithm=\"http://www.w3.org/2000/09/xmldsig#enveloped-signature\"></ds:Transform>\
             <ds:Transform Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></ds:Transform>\
             </ds:Transforms>\
             <ds:DigestMethod Algorithm=\"http://www.w3.org/2001/04/xmlenc#sha256\"></ds:DigestMethod>\
             <ds:DigestValue>{digest}</ds:DigestValue>\
             </ds:Reference>\
             </ds:SignedInfo>"
        );
        let signing_key = SigningKey::<Sha256>::new(test_key().clone());
        let signature = STANDARD.encode(signing_key.sign(signed_info.as_bytes()).to_bytes());
        let signature_element = format!(
            "<ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\">{signed_info}\
             <ds:SignatureValue>{signature}</ds:SignatureValue></ds:Signature>"
        );
        let payload = if tamper { "tampered" } else { "payload" };
        format!(
            "<t:Doc xmlns:t=\"urn:test\" ID=\"_doc1\"><t:Issuer>idp</t:Issuer>{signature_element}<t:Value>{payload}</t:Value></t:Doc>"
        )
    }

    #[test]
    fn a_valid_enveloped_signature_verifies() {
        let xml = signed_document(false);
        let doc = Document::parse(&xml).unwrap();
        let key = test_key().to_public_key();
        verify_enveloped_signature(&doc, doc.root_element(), &key).unwrap();
    }

    #[test]
    fn a_tampered_document_fails_the_digest() {
        let xml = signed_document(true);
        let doc = Document::parse(&xml).unwrap();
        let key = test_key().to_public_key();
        let error = verify_enveloped_signature(&doc, doc.root_element(), &key).unwrap_err();
        assert!(error.contains("digest mismatch"), "{error}");
    }

    #[test]
    fn a_foreign_key_fails_the_signature() {
        let xml = signed_document(false);
        let doc = Document::parse(&xml).unwrap();
        let other = RsaPrivateKey::new(&mut rand::thread_rng(), 2048)
            .unwrap()
            .to_public_key();
        let error = verify_enveloped_signature(&doc, doc.root_element(), &other).unwrap_err();
        assert!(error.contains("signature verification failed"), "{error}");
    }

    #[test]
    fn an_unsigned_element_is_rejected() {
        let doc = Document::parse(r#"<t:Doc xmlns:t="urn:test" ID="_doc1"/>"#).unwrap();
        let key = test_key().to_public_key();
        let error = verify_enveloped_signature(&doc, doc.root_element(), &key).unwrap_err();
        assert!(error.contains("does not carry an XML signature"), "{error}");
    }

    #[test]
    fn duplicate_reference_ids_are_rejected() {
        // Signature wrapping: a second element claiming the signed ID.
        let xml = signed_document(false).replace(
            "<t:Value>payload</t:Value>",
            "<t:Value>payload</t:Value><t:Evil ID=\"_doc1\"></t:Evil>",
        );
        let doc = Document::parse(&xml).unwrap();
        let key = test_key().to_public_key();
        let error = verify_enveloped_signature(&doc, doc.root_element(), &key).unwrap_err();
        assert!(error.contains("not unique"), "{error}");
    }

    #[test]
    fn certificates_yield_the_rsa_public_key() {
        use rsa::pkcs8::EncodePrivateKey;
        let pkcs8 = test_key().to_pkcs8_der().unwrap();
        let key_pair = rcgen::KeyPair::try_from(pkcs8.as_bytes()).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["idp.example.com".into()])
            .unwrap()
            .self_signed(&key_pair)
            .unwrap();
        let public = public_key_from_certificate_pem(&cert.pem()).unwrap();
        assert_eq!(public, test_key().to_public_key());
    }
}
