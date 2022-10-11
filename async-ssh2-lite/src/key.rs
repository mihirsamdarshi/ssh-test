use crypto::digest::Digest;
use crypto::md5::Md5;
use openssh_keys::PublicKey;
use openssl::pkey::Private;
use openssl::rsa::Rsa;
use pem::{encode, Pem};
fn generate_key() {
  // Generate a new 4096-bit key.
  let rsa = Rsa::generate(4096).unwrap();

  let e = rsa.e();
  let n = rsa.n();

  println!("{}", private_key_to_pem_string(&rsa));
  println!(
    "{}",
    public_key_to_string(e.to_vec(), n.to_vec(), &String::from("msamdars@test.email"))
  );
  println!("{}", fingerprint_md5_string(e.to_vec(), n.to_vec()));
}

fn private_key_to_pem_string(rsa: &Rsa<Private>) -> String {
  let private_key = rsa.private_key_to_der().unwrap();
  let private_pem = Pem {
    tag: String::from("RSA PRIVATE KEY"),
    contents: private_key,
  };

  encode(&private_pem)
}

fn public_key_to_string(e: Vec<u8>, n: Vec<u8>, comment: &str) -> String {
  let mut key = PublicKey::from_rsa(e, n);
  key.set_comment(comment);
  key.to_string()
}

fn fingerprint_md5_string(e: Vec<u8>, n: Vec<u8>) -> String {
  let key = PublicKey::from_rsa(e, n);
  let mut sh = Md5::new();
  sh.input(&key.data());
  let mut output = [0; 16];
  sh.result(&mut output);

  let md5: Vec<String> = output.iter().map(|n| format!("{:02x}", n)).collect();

  md5.join(":")
}
