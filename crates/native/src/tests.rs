use serde_json::{Value, json};

use super::call;

/// Decodes a reply: `Ok((json, bytes))` or `Err(code)`.
fn send(request: Value, data: Option<&[u8]>) -> Result<(Value, Vec<u8>), String> {
    let reply = call(request.to_string().as_bytes(), data);
    let len = u32::from_be_bytes(reply[1..5].try_into().unwrap()) as usize;
    let value: Value = serde_json::from_slice(&reply[5..5 + len]).unwrap();
    match reply[0] {
        0 => Ok((value, reply[5 + len..].to_vec())),
        _ => Err(value["code"].as_str().unwrap().to_owned()),
    }
}

fn ok(request: Value) -> Value {
    send(request.clone(), None).unwrap_or_else(|code| panic!("{request}: {code}")).0
}

/// The global state makes this one sequential scenario.
#[test]
fn calls() {
    let dir = std::env::temp_dir().join(format!("memo-android-{}", uuid::Uuid::now_v7()));
    let device = json!({ "name": "Pixel", "kind": "android" });

    assert_eq!(send(json!({ "call": "nope" }), None), Err("bad_request".into()));
    assert_eq!(send(json!({ "call": "memo_list", "filter": {} }), None), Err("locked".into()));
    assert!(ok(json!({ "call": "password_problem", "password": "short" })).is_string());
    ok(json!({ "call": "init", "dir": dir }));

    // Registration: the server's 201 is Kotlin's business; finishing opens the account.
    let started = ok(json!({
        "call": "register_start", "password": "correct horse battery staple",
        "username": " alice ", "invite": "code", "device": device,
    }));
    assert_eq!(started["request"]["username"], "alice");
    assert_eq!(started["request"]["device"]["kind"], "android");
    assert!(started["recovery_key"].as_str().unwrap().len() > 40);
    let opened = ok(json!({ "call": "register_finish" }));
    let user_id = opened["user_id"].as_str().unwrap().to_owned();
    assert_eq!(opened["secrets"]["user_id"], user_id.as_str());
    assert_eq!(send(json!({ "call": "register_finish" }), None), Err("no_pending".into()));

    let id = ok(json!({ "call": "create", "body": "- [ ] milk #Shopping", "now": 1 }));
    let list = ok(json!({ "call": "memo_list", "filter": {} }));
    assert_eq!(list[0]["title"], "milk #Shopping");
    assert_eq!(list[0]["tags"], json!(["shopping"]));
    assert!(ok(json!({ "call": "toggle_task", "memo_id": id, "offset": 2, "now": 2 })).as_bool().unwrap());
    let detail = ok(json!({ "call": "memo_detail", "memo_id": id }));
    assert_eq!(detail["body"], "- [x] milk #Shopping");
    let blocks = ok(json!({ "call": "render", "body": detail["body"] }));
    assert_eq!(blocks[0]["items"][0]["task"], json!({ "checked": true, "offset": 2 }));
    assert!(ok(json!({ "call": "has_unsynced" })).as_bool().unwrap());

    // A sync session starts with the dirty memo in its push.
    let request = ok(json!({ "call": "sync_start", "now": 3 }));
    assert_eq!(request["push"][0]["op"], "memo.put");
    assert_eq!(request["push"][0]["id"], id);
    assert_eq!(ok(json!({ "call": "sync_finish" })), json!([]));

    // An image: the original and the thumbnail travel in one buffer.
    let (meta, cleaned) = send(json!({ "call": "clean_image" }), Some(&png())).unwrap();
    assert_eq!((meta["mime"].as_str(), meta["width"].as_u64()), (Some("image/png"), Some(1)));
    assert_eq!(send(json!({ "call": "renderable", "mime": "image/png" }), Some(&cleaned)).unwrap().0, json!(true));
    assert_eq!(send(json!({ "call": "renderable", "mime": "image/jpeg" }), Some(&cleaned)).unwrap().0, json!(false));
    let mut data = cleaned.clone();
    data.extend_from_slice(&cleaned);
    let thumb = json!({ "mime": "image/png", "width": 1, "height": 1 });
    let add = json!({
        "call": "add_attachment", "memo_id": id, "mime": "image/png", "width": 1, "height": 1,
        "size": cleaned.len(), "thumb": thumb,
    });
    assert_eq!(send(add.clone(), Some(&data[..3])), Err("bad_request".into()));
    assert_eq!(send(add.clone(), Some(&[b"GIF89a".as_slice(), &data[6..]].concat())), Err("bad_image".into()));
    let attachment = send(add, Some(&data)).unwrap().0;
    let request = json!({ "call": "attachment", "memo_id": id, "attachment_id": attachment, "kind": "thumbnail" });
    let (found, bytes) = send(request, None).unwrap();
    assert_eq!((found, bytes), (json!(true), cleaned));

    // Locking forgets the keys; the keystore's secrets open the same database again.
    ok(json!({ "call": "close" }));
    assert_eq!(send(json!({ "call": "memo_detail", "memo_id": id }), None), Err("locked".into()));
    let reopened = ok(json!({ "call": "open", "secrets": opened["secrets"] }));
    assert_eq!(reopened["fingerprint"], opened["fingerprint"]);
    assert_eq!(ok(json!({ "call": "memo_detail", "memo_id": id }))["body"], "- [x] milk #Shopping");

    let mut bad = opened["secrets"].clone();
    bad["private_key"] = json!("AAAA");
    assert_eq!(send(json!({ "call": "open", "secrets": bad }), None), Err("malformed".into()));

    ok(json!({ "call": "forget", "user_id": user_id }));
    assert!(!dir.join(format!("{user_id}.db")).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// A 1×1 PNG; CRCs are not checked (see memo-core's image tests).
fn png() -> Vec<u8> {
    let chunk = |kind: &[u8; 4], data: &[u8]| [&(data.len() as u32).to_be_bytes()[..], kind, data, &[0; 4]].concat();
    [
        b"\x89PNG\r\n\x1a\n".to_vec(),
        chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]),
        chunk(b"tEXt", b"Author\0me"),
        chunk(b"IDAT", &[1, 2, 3]),
        chunk(b"IEND", &[]),
    ]
    .concat()
}
