fn main() {
    let _ = hkdf::Hkdf::<sha2::Sha256>::new(None, b"relay-must-not-derive-content-keys");
}
