use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::json;
use tempfile::tempdir;

#[path = "../src/keychain.rs"]
mod keychain;
#[path = "../src/security.rs"]
mod security;
use keychain::{
    decode_jwt_payload, extract_email_from_token_string, load_token_from_profile,
    save_token_to_profile,
};

#[test]
fn test_decode_jwt_payload() {
    let payload_json = json!({
        "email": "developer@example.com",
        "sub": "1234567890",
        "email_verified": true
    })
    .to_string();

    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(&payload_json);
    let jwt = format!("{}.{}.fake_signature", header, payload);

    let decoded = decode_jwt_payload(&jwt).expect("should decode JWT payload");
    assert_eq!(decoded["email"], "developer@example.com");
    assert_eq!(decoded["sub"], "1234567890");
}

#[test]
fn test_extract_email_from_token_string_go_keyring() {
    let payload_json = json!({
        "email": "alice@gmail.com"
    })
    .to_string();
    let jwt = format!("header.{}.sig", URL_SAFE_NO_PAD.encode(&payload_json));

    let token_obj = json!({
        "auth_method": "consumer",
        "id_token": jwt,
        "token": {
            "access_token": "ya29.dummy",
            "token_type": "Bearer"
        }
    });

    let raw_b64 = STANDARD.encode(token_obj.to_string());
    let keyring_str = format!("go-keyring-base64:{}", raw_b64);

    let email = extract_email_from_token_string(&keyring_str);
    assert_eq!(email, Some("alice@gmail.com".to_string()));
}

#[test]
fn test_extract_email_from_nested_token_json() {
    let payload_json = json!({
        "email": "bob@gmail.com"
    })
    .to_string();
    let jwt = format!("header.{}.sig", URL_SAFE_NO_PAD.encode(&payload_json));

    let token_obj = json!({
        "token": {
            "id_token": jwt,
            "access_token": "ya29.dummy"
        }
    });

    let email = extract_email_from_token_string(&token_obj.to_string());
    assert_eq!(email, Some("bob@gmail.com".to_string()));
}

#[test]
fn test_save_and_load_profile_token() {
    let temp = tempdir().unwrap();
    let profile_dir = temp.path();

    let payload_json = json!({
        "email": "carol@gmail.com"
    })
    .to_string();
    let jwt = format!("header.{}.sig", URL_SAFE_NO_PAD.encode(&payload_json));
    let token_str = json!({ "id_token": jwt }).to_string();

    let email = save_token_to_profile(profile_dir, &token_str).unwrap();
    assert_eq!(email, Some("carol@gmail.com".to_string()));

    let loaded = load_token_from_profile(profile_dir);
    assert_eq!(loaded, Some(token_str));
}
