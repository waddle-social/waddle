//! A strict, allocation-free DER reader for the few structures the verifier
//! inspects itself.
//!
//! It walks definite-length, minimally encoded TLVs with single-byte tags and
//! never builds a tree. The verifier uses it to bound extension, name and
//! CRL-entry counts before a certificate or CRL reaches a parser that
//! allocates, to split a signed object into the exact bytes its signature
//! covers, and to decode the XMPP-specific `OtherName` identifiers that
//! neither webpki nor the metadata parser validates.

pub(super) const BOOLEAN: u8 = 0x01;
pub(super) const INTEGER: u8 = 0x02;
pub(super) const BIT_STRING: u8 = 0x03;
pub(super) const OCTET_STRING: u8 = 0x04;
pub(super) const OID: u8 = 0x06;
pub(super) const UTF8_STRING: u8 = 0x0c;
pub(super) const IA5_STRING: u8 = 0x16;
pub(super) const UTC_TIME: u8 = 0x17;
pub(super) const GENERALIZED_TIME: u8 = 0x18;
pub(super) const SEQUENCE: u8 = 0x30;
/// `[0]`, constructed: an explicit tag, or an implicitly tagged SEQUENCE.
pub(super) const CONTEXT_0: u8 = 0xa0;
/// `[1]`, constructed.
pub(super) const CONTEXT_1: u8 = 0xa1;
/// `[2]`, constructed.
pub(super) const CONTEXT_2: u8 = 0xa2;
/// `[3]`, constructed.
pub(super) const CONTEXT_3: u8 = 0xa3;
/// `[1]`, primitive.
pub(super) const CONTEXT_1_PRIMITIVE: u8 = 0x81;
/// `[2]`, primitive.
pub(super) const CONTEXT_2_PRIMITIVE: u8 = 0x82;

/// id-ce-subjectAltName, 2.5.29.17.
pub(super) const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
/// id-ce-nameConstraints, 2.5.29.30.
pub(super) const OID_NAME_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x1e];

/// The input is not the strict DER this reader accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DerError;

/// One tag-length-value.
#[derive(Debug, Clone, Copy)]
pub(super) struct Tlv<'a> {
    pub(super) tag: u8,
    pub(super) contents: &'a [u8],
    /// The whole element, tag and length included.
    pub(super) encoded: &'a [u8],
}

/// Reads consecutive TLVs from a byte slice.
#[derive(Debug, Clone, Copy)]
pub(super) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(super) fn new(input: &'a [u8]) -> Self {
        Self { rest: input }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    pub(super) fn peek_tag(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// Read the next element, whatever its tag.
    pub(super) fn read(&mut self) -> Result<Tlv<'a>, DerError> {
        let input = self.rest;
        let (&tag, after_tag) = input.split_first().ok_or(DerError)?;
        // Tag numbers of 31 and above use further tag bytes; nothing the
        // verifier reads has one.
        if tag & 0x1f == 0x1f {
            return Err(DerError);
        }
        let (&first, after_first) = after_tag.split_first().ok_or(DerError)?;
        let (length, after_length) = if first < 0x80 {
            (usize::from(first), after_first)
        } else {
            let width = usize::from(first & 0x7f);
            // 0x80 is the indefinite form, which DER forbids. Four length
            // bytes cover every size the verifier's limits allow.
            if width == 0 || width > 4 || after_first.len() < width {
                return Err(DerError);
            }
            let (digits, after_digits) = after_first.split_at(width);
            if digits[0] == 0 {
                return Err(DerError);
            }
            let length = digits
                .iter()
                .fold(0usize, |length, &digit| (length << 8) | usize::from(digit));
            // The long form is only valid for lengths the short form
            // cannot express.
            if length < 0x80 {
                return Err(DerError);
            }
            (length, after_digits)
        };
        if after_length.len() < length {
            return Err(DerError);
        }
        let (contents, rest) = after_length.split_at(length);
        let encoded_length = input.len() - rest.len();
        self.rest = rest;
        Ok(Tlv {
            tag,
            contents,
            encoded: &input[..encoded_length],
        })
    }

    /// Read the next element and require its tag.
    pub(super) fn expect(&mut self, tag: u8) -> Result<Tlv<'a>, DerError> {
        let element = self.read()?;
        if element.tag == tag {
            Ok(element)
        } else {
            Err(DerError)
        }
    }

    /// Require that nothing is left.
    pub(super) fn finish(self) -> Result<(), DerError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(DerError)
        }
    }
}

/// The contents of `input` when it is exactly one element with `tag` and
/// nothing after it.
pub(super) fn single(input: &[u8], tag: u8) -> Result<&[u8], DerError> {
    let mut reader = Reader::new(input);
    let element = reader.expect(tag)?;
    reader.finish()?;
    Ok(element.contents)
}

/// Count the elements in `contents`.
fn count_elements(contents: &[u8]) -> Result<usize, DerError> {
    let mut reader = Reader::new(contents);
    let mut count = 0usize;
    while !reader.is_empty() {
        reader.read()?;
        count = count.saturating_add(1);
    }
    Ok(count)
}

/// The three parts of a certificate or CRL: the signed bytes, the outer
/// signature algorithm and the signature.
#[derive(Debug, Clone, Copy)]
pub(super) struct SignedObject<'a> {
    /// The to-be-signed element, tag and length included: the exact bytes
    /// the signature covers.
    pub(super) signed: &'a [u8],
    /// The contents of the outer `AlgorithmIdentifier`, the form in which
    /// rustls signature algorithms identify themselves.
    pub(super) algorithm: &'a [u8],
    pub(super) signature: &'a [u8],
}

/// Split `SEQUENCE { tbs, signatureAlgorithm, signatureValue }`.
pub(super) fn signed_object(der: &[u8]) -> Result<SignedObject<'_>, DerError> {
    let mut outer = Reader::new(single(der, SEQUENCE)?);
    let signed = outer.expect(SEQUENCE)?;
    let algorithm = outer.expect(SEQUENCE)?;
    let signature = outer.expect(BIT_STRING)?;
    outer.finish()?;
    // A signature is a whole number of octets: no unused bits.
    let (&unused_bits, signature) = signature.contents.split_first().ok_or(DerError)?;
    if unused_bits != 0 {
        return Err(DerError);
    }
    Ok(SignedObject {
        signed: signed.encoded,
        algorithm: algorithm.contents,
        signature,
    })
}

/// How many variable-length lists a certificate carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CertificateShape {
    pub(super) extensions: usize,
    pub(super) subject_alt_names: usize,
    pub(super) name_constraint_subtrees: usize,
}

/// Walk a certificate without allocating and count its extensions,
/// subjectAltName entries and nameConstraints subtrees.
pub(super) fn certificate_shape(der: &[u8]) -> Result<CertificateShape, DerError> {
    let object = signed_object(der)?;
    let mut tbs = Reader::new(single(object.signed, SEQUENCE)?);
    if tbs.peek_tag() == Some(CONTEXT_0) {
        tbs.read()?;
    }
    tbs.expect(INTEGER)?;
    // signature, issuer, validity, subject, subjectPublicKeyInfo.
    for _ in 0..5 {
        tbs.expect(SEQUENCE)?;
    }
    // issuerUniqueID and subjectUniqueID.
    while matches!(
        tbs.peek_tag(),
        Some(CONTEXT_1 | CONTEXT_2 | CONTEXT_1_PRIMITIVE | CONTEXT_2_PRIMITIVE)
    ) {
        tbs.read()?;
    }
    let mut shape = CertificateShape::default();
    if tbs.is_empty() {
        return Ok(shape);
    }
    let wrapper = tbs.expect(CONTEXT_3)?;
    tbs.finish()?;
    let mut extensions = Reader::new(single(wrapper.contents, SEQUENCE)?);
    while !extensions.is_empty() {
        let extension = extensions.expect(SEQUENCE)?;
        shape.extensions = shape.extensions.saturating_add(1);
        let mut fields = Reader::new(extension.contents);
        let identifier = fields.expect(OID)?;
        if fields.peek_tag() == Some(BOOLEAN) {
            fields.read()?;
        }
        let value = fields.expect(OCTET_STRING)?;
        fields.finish()?;
        if identifier.contents == OID_SUBJECT_ALT_NAME {
            let names = count_elements(single(value.contents, SEQUENCE)?)?;
            shape.subject_alt_names = shape.subject_alt_names.saturating_add(names);
        } else if identifier.contents == OID_NAME_CONSTRAINTS {
            let forms = name_constraint_forms(single(value.contents, SEQUENCE)?)?;
            shape.name_constraint_subtrees = shape
                .name_constraint_subtrees
                .saturating_add(forms.subtrees);
        }
    }
    Ok(shape)
}

/// What a nameConstraints extension constrains.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct NameConstraintForms {
    /// Permitted and excluded subtrees together.
    pub(super) subtrees: usize,
    /// Whether any subtree constrains the `otherName` form, which carries
    /// SRVName (RFC 4985) and XmppAddr identifiers.
    pub(super) other_name: bool,
}

/// Inspect the contents of a `NameConstraints` SEQUENCE: the bytes after its
/// tag and length, which is also how a webpki trust anchor stores them.
pub(super) fn name_constraint_forms(contents: &[u8]) -> Result<NameConstraintForms, DerError> {
    let mut forms = NameConstraintForms::default();
    let mut lists = Reader::new(contents);
    while !lists.is_empty() {
        let list = lists.read()?;
        // permittedSubtrees [0] and excludedSubtrees [1].
        if list.tag != CONTEXT_0 && list.tag != CONTEXT_1 {
            return Err(DerError);
        }
        let mut subtrees = Reader::new(list.contents);
        while !subtrees.is_empty() {
            let subtree = subtrees.expect(SEQUENCE)?;
            let base = Reader::new(subtree.contents).read()?;
            forms.subtrees = forms.subtrees.saturating_add(1);
            // GeneralName is a CHOICE of context-specific tags; otherName
            // is [0].
            if base.tag & 0xc0 != 0x80 {
                return Err(DerError);
            }
            if base.tag & 0x1f == 0 {
                forms.other_name = true;
            }
        }
    }
    Ok(forms)
}

/// The string inside an `OtherName` value.
///
/// `value` is what follows the type-id inside the `OtherName`: it must be
/// exactly `[0] EXPLICIT` around exactly one string with `string_tag`, with
/// no trailing bytes at either level.
pub(super) fn other_name_string(value: &[u8], string_tag: u8) -> Result<&[u8], DerError> {
    single(single(value, CONTEXT_0)?, string_tag)
}

/// Count the revoked-certificate entries of a CRL without allocating.
pub(super) fn crl_entry_count(der: &[u8]) -> Result<usize, DerError> {
    let object = signed_object(der)?;
    let mut tbs = Reader::new(single(object.signed, SEQUENCE)?);
    if tbs.peek_tag() == Some(INTEGER) {
        tbs.read()?;
    }
    // signature and issuer.
    tbs.expect(SEQUENCE)?;
    tbs.expect(SEQUENCE)?;
    // thisUpdate, then the optional nextUpdate.
    let this_update = tbs.read()?;
    if this_update.tag != UTC_TIME && this_update.tag != GENERALIZED_TIME {
        return Err(DerError);
    }
    if matches!(tbs.peek_tag(), Some(UTC_TIME | GENERALIZED_TIME)) {
        tbs.read()?;
    }
    if tbs.peek_tag() == Some(SEQUENCE) {
        count_elements(tbs.read()?.contents)
    } else {
        Ok(0)
    }
}
