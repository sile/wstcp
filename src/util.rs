use base64;
use sha1::{Digest, Sha1};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn calc_accept_hash(key: &str) -> String {
    let mut sh = Sha1::default();
    sh.input(format!("{}{}", key, GUID).as_bytes());
    let output = sh.result();
    base64::encode(&output)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn it_works() {
        let hash = calc_accept_hash("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(hash, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
