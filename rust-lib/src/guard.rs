//! Requests a wallet answers with its seed's unlock key: an `eth-sign-request` under the reserved
//! purpose `m/1280263247'`, where the signer interface puts those keys. Whoever reads the answer can
//! recover the key, so only the QR signer module may show one.

use minicbor::data::Type;
use minicbor::Decoder;

/// "LOGO" in ASCII, as the signer interface's `unlock::PURPOSE`.
pub const PURPOSE: u32 = 1_280_263_247;
/// The one module that asks a wallet for its seed's unlock key.
pub const OPENER: &str = "qr_signer_module";

const TYPE: &str = "eth-sign-request";
const HARDENED: u32 = 0x8000_0000;

/// Whether these parts show an `eth-sign-request` under the reserved purpose. One that can't be
/// read counts as one: it is refused rather than shown unchecked.
pub fn asks_unlock_key(parts: &[String]) -> bool {
    let mut multi = ur::Decoder::default();
    let mut fountain = false;
    for part in parts {
        let lower = part.trim().to_ascii_lowercase();
        let Some((ur_type, rest)) = lower
            .strip_prefix("ur:")
            .and_then(|body| body.split_once('/'))
        else {
            continue;
        };
        if ur_type != TYPE {
            continue;
        }
        if rest.contains('/') {
            fountain = true;
            if multi.receive(&lower).is_err() {
                return true;
            }
        } else {
            match ur::ur::decode(&lower) {
                Ok((_, cbor)) if !reserved(&cbor).unwrap_or(true) => {}
                _ => return true,
            }
        }
    }
    if !fountain {
        return false;
    }
    if !multi.complete() {
        return true;
    }
    match multi.message() {
        Ok(Some(cbor)) => reserved(&cbor).unwrap_or(true),
        _ => true,
    }
}

/// Whether a request's derivation path (key 5, a `crypto-keypath`) starts at the reserved purpose,
/// hardened or not. None when it can't be read.
fn reserved(cbor: &[u8]) -> Option<bool> {
    let mut d = Decoder::new(cbor);
    let entries = d.map().ok()??;
    for _ in 0..entries {
        if d.u64().ok()? != 5 {
            d.skip().ok()?;
            continue;
        }
        if d.datatype().ok()? == Type::Tag {
            d.tag().ok()?;
        }
        let fields = d.map().ok()??;
        for _ in 0..fields {
            if d.u64().ok()? != 1 {
                d.skip().ok()?;
                continue;
            }
            // An empty path names no key; a wildcard or a range first is not a path to sign at.
            if d.array().ok()?? == 0 {
                return Some(false);
            }
            return match d.datatype().ok()? {
                Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                    Some(d.u32().ok()? & !HARDENED == PURPOSE)
                }
                _ => None,
            };
        }
        return Some(false);
    }
    Some(false)
}

#[cfg(test)]
pub mod tests {
    use minicbor::data::Tag;
    use minicbor::Encoder;

    use super::*;

    /// An `eth-sign-request` for `personal_sign` at `path`, as the QR signer writes one.
    pub fn request(path: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out);
        e.map(4).unwrap();
        e.u8(1)
            .unwrap()
            .tag(Tag::new(37))
            .unwrap()
            .bytes(&[7; 16])
            .unwrap();
        e.u8(2).unwrap().bytes(b"sign this").unwrap();
        e.u8(3).unwrap().u8(3).unwrap();
        e.u8(5).unwrap().tag(Tag::new(304)).unwrap().map(2).unwrap();
        e.u8(1).unwrap().array(path.len() as u64 * 2).unwrap();
        for c in path {
            e.u32(c & !HARDENED)
                .unwrap()
                .bool(c & HARDENED != 0)
                .unwrap();
        }
        e.u8(2).unwrap().u32(0x73c5_da0a).unwrap();
        out
    }

    pub fn single(cbor: &[u8]) -> Vec<String> {
        vec![ur::encode(cbor, &ur::Type::Custom(TYPE)).to_uppercase()]
    }

    /// Fountain parts, every plain one then as many again, as the QR signer shows a long request.
    pub fn fountain(cbor: &[u8]) -> Vec<String> {
        let mut e = ur::Encoder::new(cbor, 20, TYPE).unwrap();
        (0..2 * e.fragment_count())
            .map(|_| e.next_part().unwrap().to_uppercase())
            .collect()
    }

    const UNLOCK: [u32; 2] = [HARDENED | PURPOSE, HARDENED];
    const EVM: [u32; 5] = [HARDENED | 44, HARDENED | 60, HARDENED, 0, 0];

    #[test]
    fn a_request_under_the_reserved_purpose_is_found_whole_or_in_parts() {
        assert!(asks_unlock_key(&single(&request(&UNLOCK))));
        assert!(asks_unlock_key(&fountain(&request(&UNLOCK))));
        assert!(
            asks_unlock_key(&single(&request(&[PURPOSE, 0]))),
            "unhardened too"
        );
        assert!(!asks_unlock_key(&single(&request(&EVM))));
        assert!(!asks_unlock_key(&fountain(&request(&EVM))));
        assert!(!asks_unlock_key(&single(&request(&[]))));
    }

    #[test]
    fn one_hidden_among_others_is_found() {
        let mut parts = single(&request(&EVM));
        parts.extend(single(&request(&UNLOCK)));
        assert!(asks_unlock_key(&parts));
    }

    #[test]
    fn other_codes_are_not_its_business_but_an_unreadable_request_is() {
        assert!(!asks_unlock_key(&["UR:BYTES/AA".into(), "hello".into()]));
        assert!(!asks_unlock_key(&[]));
        assert!(asks_unlock_key(
            &["UR:ETH-SIGN-REQUEST/NOTBYTEWORDS".into()]
        ));
        let mut half = fountain(&request(&EVM));
        half.truncate(1);
        assert!(
            asks_unlock_key(&half),
            "a request not all there is not shown"
        );
    }
}
