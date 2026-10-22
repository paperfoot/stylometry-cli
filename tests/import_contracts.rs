//! Import contracts: every real short-form export shape is read as the
//! owner's own writing, with item kinds, ids, ISO dates and media flags.
//!
//! Fixtures are synthetic (written here, no real posts) and mirror the
//! shapes of an unzipped X archive, xmaster dumps, CSV and JSONL exports.
//! Each test builds a profile in an isolated `STYLOMETRY_DATA_DIR` and reads
//! the saved items back.

use std::path::Path;

use assert_cmd::Command;
use serde_json::{Value, json};

fn bin() -> Command {
    Command::cargo_bin("stylometry").unwrap()
}

const TOPICS: [&str; 8] = [
    "shipping small tools",
    "writing every morning",
    "reading old letters",
    "fixing the garden fence",
    "testing a new keyboard",
    "cooking for friends",
    "walking by the river",
    "learning a little piano",
];

/// A synthetic post body: varied enough to build a profile.
fn post(i: usize) -> String {
    format!(
        "Day {i} of {} and it keeps teaching me something. Small steps, every day, add up.",
        TOPICS[i % TOPICS.len()]
    )
}

/// Build profile `name` from `corpus`; returns the saved profile JSON.
fn build(data: &Path, corpus: &Path, name: &str, extra: &[&str]) -> Value {
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data)
        .args(["profile", "build", name, "--json", "--corpus"])
        .arg(corpus)
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "build failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let env: Value = serde_json::from_slice(&out.stdout).unwrap();
    let path = env["data"]["path"].as_str().unwrap().to_string();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// (kind, text, meta) of every saved item.
fn items(p: &Value) -> Vec<(String, String, Value)> {
    p["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let meta = c["meta"].clone();
            let kind = meta["kind"].as_str().unwrap_or("post").to_string();
            let paras: Vec<&str> = c["paragraphs"].as_array().unwrap().iter().filter_map(|p| p.as_str()).collect();
            (kind, paras.join("\n\n"), meta)
        })
        .collect()
}

fn count(items: &[(String, String, Value)], kind: &str) -> usize {
    items.iter().filter(|i| i.0 == kind).count()
}

fn find<'a>(items: &'a [(String, String, Value)], id: &str) -> &'a (String, String, Value) {
    items.iter().find(|i| i.2["id"] == id).unwrap_or_else(|| panic!("no item with id {id}"))
}

fn ytd(name: &str, v: &Value) -> String {
    format!("window.YTD.{name}.part0 = {}", serde_json::to_string_pretty(v).unwrap())
}

fn tweet(id: u64, text: &str, extra: Value) -> Value {
    let mut t = json!({
        "id_str": id.to_string(),
        "full_text": text,
        "favorite_count": "3",
        "retweet_count": "1",
        "created_at": format!("Wed Oct {:02} 20:19:24 +0000 2018", 1 + id % 28),
        "display_text_range": ["0", text.chars().count().to_string()],
    });
    if let (Some(o), Some(e)) = (t.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    json!({ "tweet": t })
}

/// An unzipped archive: data/ with tweets.js, tweets-part1.js, account.js,
/// note-tweet.js, and the files that must be left out.
fn write_archive(root: &Path) {
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut part0: Vec<Value> = (0..22).map(|i| tweet(100 + i as u64, &post(i), json!({}))).collect();
    // A reply to someone else: X put two handles in front of it.
    let reply = "@alice @bob_2 Agreed, and the second point matters more than it looks.";
    part0.push(tweet(
        200,
        reply,
        json!({"in_reply_to_status_id_str": "9", "in_reply_to_user_id_str": "77", "in_reply_to_screen_name": "alice", "display_text_range": ["14", reply.chars().count().to_string()]}),
    ));
    // A self-reply continuing the owner's own post 100.
    part0.push(tweet(
        201,
        "And the part I left out yesterday: the fence needed new posts too.",
        json!({"in_reply_to_status_id_str": "100", "in_reply_to_user_id_str": "4242", "in_reply_to_screen_name": "owner"}),
    ));
    // A retweet: not the owner's writing.
    part0.push(tweet(202, "RT @someone: a retweeted line that is not mine at all", json!({})));
    // An image post: the media link is not text.
    part0.push(tweet(
        203,
        "Morning light over the river today &amp; it was cold https://t.co/IMG203",
        json!({"entities": {"media": [{"url": "https://t.co/IMG203"}]}, "extended_entities": {"media": [{"url": "https://t.co/IMG203"}]}}),
    ));
    // A quote post: the quoted post's link is not text.
    part0.push(tweet(
        204,
        "This is the clearest version of the idea so far https://t.co/Q204",
        json!({"entities": {"urls": [{"url": "https://t.co/Q204", "expanded_url": "https://twitter.com/someone/status/55"}]}}),
    ));
    // A long post, truncated in tweets.js; the full text is in note-tweet.js.
    part0.push(tweet(
        205,
        "Long note on writing every morning for a year: the first month was the hardest part, and… https://t.co/N205",
        json!({}),
    ));
    std::fs::write(data.join("tweets.js"), ytd("tweets", &Value::Array(part0))).unwrap();
    let part1: Vec<Value> = (22..26).map(|i| tweet(300 + i as u64, &post(i), json!({}))).collect();
    std::fs::write(data.join("tweets-part1.js"), ytd("tweets", &Value::Array(part1))).unwrap();
    std::fs::write(
        data.join("account.js"),
        ytd("account", &json!([{"account": {"email": "owner@example.invalid", "username": "owner", "accountId": "4242", "accountDisplayName": "Owner"}}])),
    )
    .unwrap();
    std::fs::write(
        data.join("note-tweet.js"),
        ytd(
            "note_tweet",
            &json!([{"noteTweet": {"noteTweetId": "1", "createdAt": "2018-10-10T20:19:24.000Z", "core": {"text": "Long note on writing every morning for a year: the first month was the hardest part, and then it became the easiest hour of the day."}}}]),
        ),
    )
    .unwrap();
    std::fs::write(
        data.join("deleted-tweets.js"),
        ytd("deleted_tweets", &json!([tweet(400, "DELETEDMARK a post the owner deleted for a reason", json!({}))])),
    )
    .unwrap();
    std::fs::write(
        data.join("like.js"),
        ytd("like", &json!([{"like": {"tweetId": "1", "fullText": "LIKEMARK someone else's post that was liked"}}])),
    )
    .unwrap();
    std::fs::write(
        data.join("direct-messages.js"),
        ytd(
            "direct_messages",
            &json!([{"dmConversation": {"conversationId": "4242-77", "messages": [
                {"messageCreate": {"senderId": "77", "recipientId": "4242", "id": "d1", "text": "DMOTHER are you coming to the thing later", "createdAt": "2018-10-02T10:00:00.000Z", "mediaUrls": []}},
                {"messageCreate": {"senderId": "4242", "recipientId": "77", "id": "d2", "text": "DMOWN yes, see you there at six", "createdAt": "2018-10-02T10:02:00.000Z", "mediaUrls": []}}
            ]}}]),
        ),
    )
    .unwrap();
    // Unrelated archive files are ignored.
    std::fs::write(data.join("tweet-headers.js"), ytd("tweet_headers", &json!([{"tweet": {"tweet_id": "1"}}]))).unwrap();
    std::fs::write(data.join("manifest.js"), "window.__THAR_CONFIG = {};").unwrap();
}

#[test]
fn x_archive_folder_reads_only_the_owners_posts_with_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    write_archive(&tmp.path().join("archive"));
    let p = build(&tmp.path().join("data"), &tmp.path().join("archive"), "x", &[]);
    assert_eq!(p["register"], "social");
    let it = items(&p);
    // 26 posts + reply + thread part + media + quote + note; no retweet.
    assert_eq!(it.len(), 31, "{:#?}", it.iter().map(|i| &i.1).collect::<Vec<_>>());
    assert_eq!(count(&it, "reply"), 1);
    assert_eq!(count(&it, "thread_part"), 1);
    assert_eq!(count(&it, "quote"), 1);
    assert_eq!(count(&it, "post"), 28);

    let all: String = it.iter().map(|i| i.1.as_str()).collect::<Vec<_>>().join("\n");
    for mark in ["RT @", "DELETEDMARK", "LIKEMARK", "DMOTHER", "DMOWN", "t.co/IMG", "t.co/Q204", "&amp;"] {
        assert!(!all.contains(mark), "{mark} leaked into the profile");
    }

    let reply = find(&it, "200");
    assert_eq!(reply.1, "Agreed, and the second point matters more than it looks.");
    assert_eq!(reply.2["parent_id"], "9");
    assert_eq!(find(&it, "201").0, "thread_part");
    let media = find(&it, "203");
    assert_eq!(media.1, "Morning light over the river today & it was cold");
    assert_eq!(media.2["media"], true);
    assert_eq!(find(&it, "204").1, "This is the clearest version of the idea so far");
    assert!(find(&it, "205").1.ends_with("the easiest hour of the day."), "note-tweet text replaces the truncated one");

    let first = find(&it, "100");
    assert_eq!(first.2["likes"], 3.0);
    assert_eq!(first.2["reposts"], 1.0);
    assert!(first.2.get("impressions").is_none());
    let dates: Vec<&str> = it.iter().map(|i| i.2["date"].as_str().unwrap()).collect();
    assert!(dates.iter().all(|d| d.len() == 20 && d.ends_with('Z') && d.as_bytes()[10] == b'T'), "{dates:?}");
    assert!(dates.windows(2).all(|w| w[0] <= w[1]), "items are in time order");
}

#[test]
fn older_archive_tweet_js_passed_as_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("old/data");
    std::fs::create_dir_all(&data).unwrap();
    let mut tweets: Vec<Value> = (0..21).map(|i| tweet(500 + i as u64, &post(i), json!({}))).collect();
    // No account.js: a reply to one of the owner's own ids is a thread part.
    tweets.push(tweet(600, "@owner and one more thing about the fence and the posts", json!({"in_reply_to_status_id_str": "500"})));
    tweets.push(tweet(601, "@carol thanks, that is a kind thing to say today", json!({"in_reply_to_status_id_str": "1"})));
    std::fs::write(data.join("tweet.js"), ytd("tweet", &Value::Array(tweets))).unwrap();
    let p = build(&tmp.path().join("store"), &data.join("tweet.js"), "old", &[]);
    let it = items(&p);
    assert_eq!(it.len(), 23);
    assert_eq!(find(&it, "600").0, "thread_part");
    assert_eq!(find(&it, "600").1, "and one more thing about the fence and the posts");
    assert_eq!(find(&it, "601").0, "reply");
    assert_eq!(find(&it, "601").1, "thanks, that is a kind thing to say today");
}

#[test]
fn archive_dms_keep_only_the_owners_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("arch/data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("account.js"),
        ytd("account", &json!([{"account": {"username": "owner", "accountId": "4242"}}])),
    )
    .unwrap();
    let mut msgs = Vec::new();
    for i in 0..24 {
        msgs.push(json!({"messageCreate": {"senderId": "4242", "id": format!("m{i}"), "text": format!("OWN sure, {} sounds good to me", TOPICS[i % 8]), "createdAt": format!("2020-01-{:02}T10:00:00.000Z", 1 + i), "mediaUrls": []}}));
        msgs.push(json!({"messageCreate": {"senderId": "77", "id": format!("o{i}"), "text": format!("OTHER what about {} this week", TOPICS[i % 8]), "createdAt": format!("2020-01-{:02}T10:01:00.000Z", 1 + i), "mediaUrls": []}}));
    }
    std::fs::write(
        data.join("direct-messages.js"),
        ytd("direct_messages", &json!([{"dmConversation": {"conversationId": "4242-77", "messages": msgs}}])),
    )
    .unwrap();
    let p = build(&tmp.path().join("store"), &data.join("direct-messages.js"), "dm", &[]);
    assert_eq!(p["register"], "message");
    let it = items(&p);
    assert_eq!(it.len(), 24);
    assert!(it.iter().all(|i| i.1.starts_with("OWN ")), "the other side's messages leaked");
}

fn xm_row(id: usize, author: &str, text: &str, impressions: u64) -> Value {
    json!({"author": author, "date": format!("2026-10-{:02}T14:49:58.000Z", 1 + id % 28), "id": id.to_string(), "impressions": impressions, "likes": 2, "retweets": 1, "text": text})
}

#[test]
fn xmaster_dumps_keep_the_dominant_author_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("xm");
    std::fs::create_dir_all(&dir).unwrap();
    let mut a: Vec<Value> = (1..19).map(|i| xm_row(i, "@me", &post(i), 100)).collect();
    a.push(xm_row(50, "@stranger", "OTHERAUTHOR a home timeline post from someone else", 900));
    a.push(xm_row(51, "@me", "@pal @friend Thanks -- that came from my own usage last week", 0));
    let mut b: Vec<Value> = (10..25).map(|i| xm_row(i, "@me", &post(i), 100)).collect(); // ids 10..18 repeat
    b.push(xm_row(52, "@stranger", "OTHERAUTHOR another post from the same stranger", 900));
    let env = |rows: Vec<Value>| json!({"version": "1", "status": "success", "data": {"tweets": rows}});
    std::fs::write(dir.join("a.json"), env(a).to_string()).unwrap();
    std::fs::write(dir.join("b.json"), env(b).to_string()).unwrap();
    let p = build(&tmp.path().join("store"), &dir, "xm", &[]);
    assert_eq!(p["register"], "social");
    let it = items(&p);
    assert_eq!(it.len(), 25, "24 distinct posts + 1 reply");
    let ids: std::collections::HashSet<&str> = it.iter().map(|i| i.2["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), it.len(), "ids are unique across files");
    assert!(!it.iter().any(|i| i.1.contains("OTHERAUTHOR")));
    let reply = find(&it, "51");
    assert_eq!(reply.0, "reply");
    assert_eq!(reply.1, "Thanks -- that came from my own usage last week");
    assert!(reply.2.get("impressions").is_none(), "impressions 0 means unknown");
    assert_eq!(find(&it, "1").2["impressions"], 100.0);
    assert_eq!(find(&it, "1").2["reposts"], 1.0);
}

#[test]
fn csv_columns_give_ids_kinds_and_parents() {
    let tmp = tempfile::tempdir().unwrap();
    let mut csv = String::from("id,text,likes,date,parent_id,kind,is_reply\n");
    for i in 0..20 {
        // Empty parent/kind/is_reply cells must not make every row a reply.
        csv.push_str(&format!("{},\"{}\",4,2025-01-{:02},,,\n", i + 1, post(i), 1 + i));
    }
    csv.push_str("31,\"@x and this follows my own first post, as promised\",1,2025-02-01,1,,\n");
    csv.push_str("32,\"fine by me, ship it on monday then\",0,2025-02-02,,,true\n");
    csv.push_str("33,\"@carol is right, this opener names her on purpose\",0,2025-02-03,,post,\n");
    std::fs::write(tmp.path().join("posts.csv"), csv).unwrap();
    let p = build(&tmp.path().join("store"), &tmp.path().join("posts.csv"), "csvp", &[]);
    let it = items(&p);
    assert_eq!(it.len(), 23);
    assert_eq!(count(&it, "post"), 21);
    let part = find(&it, "31");
    assert_eq!(part.0, "thread_part", "a reply to the author's own id continues a thread");
    assert_eq!(part.1, "and this follows my own first post, as promised");
    assert_eq!(part.2["parent_id"], "1");
    assert_eq!(find(&it, "32").0, "reply");
    assert_eq!(find(&it, "33").1, "@carol is right, this opener names her on purpose");
    assert_eq!(find(&it, "1").2["date"], "2025-01-01");
}

#[test]
fn jsonl_kinds_and_unix_dates() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = String::new();
    for i in 0..20 {
        s.push_str(&json!({"id": 1000 + i, "content": post(i), "timestamp": 1_539_202_764 + i * 86_400}).to_string());
        s.push('\n');
    }
    s.push_str(&json!({"id": 2000, "content": "quoting this because it says it better than me", "kind": "quote"}).to_string());
    s.push('\n');
    std::fs::write(tmp.path().join("posts.jsonl"), s).unwrap();
    let p = build(&tmp.path().join("store"), &tmp.path().join("posts.jsonl"), "jl", &[]);
    let it = items(&p);
    assert_eq!(it.len(), 21);
    assert_eq!(find(&it, "2000").0, "quote");
    assert_eq!(find(&it, "1000").2["date"], "2018-10-10T20:19:24Z");
}

#[test]
fn unreadable_export_in_a_folder_is_skipped_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mixed");
    std::fs::create_dir_all(&dir).unwrap();
    let rows: Vec<Value> = (1..22).map(|i| xm_row(i, "@me", &post(i), 10)).collect();
    std::fs::write(dir.join("good.json"), json!({"data": {"tweets": rows}}).to_string()).unwrap();
    std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
    let p = build(&tmp.path().join("store"), &dir, "mixed", &[]);
    assert_eq!(items(&p).len(), 21);
    // Passed directly, the same broken file is an input error (exit 3).
    bin()
        .env("STYLOMETRY_DATA_DIR", tmp.path().join("store"))
        .args(["profile", "build", "broken", "--json", "--corpus"])
        .arg(dir.join("broken.json"))
        .assert()
        .code(3);
}

#[test]
#[ignore = "needs profile.rs to call import::split_items_for(text, register) (integrator)"]
fn email_txt_files_stay_whole_without_separators() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mail");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..22 {
        let body = format!("Hi Tom,\n\nThanks for the note about {}. I will send the draft by Friday.\n\nBest,\nB\n", TOPICS[i % 8]);
        std::fs::write(dir.join(format!("{i:02}.txt")), body).unwrap();
    }
    let p = build(&tmp.path().join("store"), &dir, "mail", &["--register", "email"]);
    let it = items(&p);
    assert_eq!(it.len(), 22, "one item per email file");
    assert!(it.iter().all(|i| i.1.starts_with("Hi Tom,") && i.1.ends_with("Best,\nB")));
}

/// Casual X-style posts: lowercase start, no final period. Paragraph repair
/// for broken conversions must not glue them into one item.
fn casual_posts(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("day {i} of {} and honestly it keeps teaching me stuff 🚀", TOPICS[i % TOPICS.len()]))
        .collect()
}

/// Run `profile build` and return (exit code, envelope); error envelopes
/// are on stderr.
fn try_build(data: &Path, corpus: &Path, name: &str, extra: &[&str]) -> (i32, Value) {
    let out = bin()
        .env("STYLOMETRY_DATA_DIR", data)
        .args(["profile", "build", name, "--json", "--corpus"])
        .arg(corpus)
        .args(extra)
        .output()
        .unwrap();
    let body = if out.stdout.is_empty() { &out.stderr } else { &out.stdout };
    (out.status.code().unwrap_or(-1), serde_json::from_slice(body).unwrap_or(Value::Null))
}

#[test]
fn separated_lowercase_posts_stay_separate_items() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("social_sep.txt");
    std::fs::write(&f, casual_posts(24).join("\n\n---\n\n")).unwrap();
    let p = build(&tmp.path().join("store"), &f, "sep", &["--register", "social"]);
    let it = items(&p);
    assert_eq!(it.len(), 24);
    assert!(it.iter().all(|i| i.1.starts_with("day ") && !i.1.contains("---")), "{:?}", it.iter().map(|i| &i.1).collect::<Vec<_>>());
}

#[test]
#[ignore = "needs commands/profile.rs to call import::read_corpus_as(&corpus, register) (integrator)"]
fn blank_line_lowercase_posts_stay_separate_items() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("social_blank.txt");
    std::fs::write(&f, casual_posts(24).join("\n\n")).unwrap();
    let p = build(&tmp.path().join("store"), &f, "blank", &["--register", "social"]);
    assert_eq!(items(&p).len(), 24);
}

#[test]
fn dm_only_folder_says_how_to_profile_the_dms() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("arch/data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("account.js"), ytd("account", &json!([{"account": {"username": "owner", "accountId": "4242"}}]))).unwrap();
    let msgs: Vec<Value> = (0..24)
        .map(|i| json!({"messageCreate": {"senderId": "4242", "id": format!("m{i}"), "text": format!("sure, {} sounds good to me", TOPICS[i % 8]), "createdAt": "2020-01-01T10:00:00.000Z"}}))
        .collect();
    std::fs::write(data.join("direct-messages.js"), ytd("direct_messages", &json!([{"dmConversation": {"messages": msgs}}]))).unwrap();
    let (code, env) = try_build(&tmp.path().join("store"), &tmp.path().join("arch"), "dm", &[]);
    assert_eq!(code, 3);
    let msg = env["error"]["message"].as_str().unwrap();
    let fix = env["error"]["suggestion"].as_str().unwrap();
    assert!(msg.contains("direct-messages.js"), "{msg}");
    assert!(fix.contains("--corpus") && fix.contains("direct-messages.js"), "{fix}");
    // The suggestion, followed literally, builds the DM profile.
    let p = build(&tmp.path().join("store"), &data.join("direct-messages.js"), "dm", &[]);
    assert_eq!(p["register"], "message");
    assert_eq!(items(&p).len(), 24);
}

#[test]
fn folder_of_failed_exports_reports_the_real_cause() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("exports");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.csv"), "title,words\nfoo,12\nbar,30\n").unwrap();
    let (code, env) = try_build(&tmp.path().join("store"), &dir, "x", &[]);
    assert_eq!(code, 3);
    assert!(env["error"]["message"].as_str().unwrap().contains("no text column"), "{env}");
}

#[test]
fn lone_tweets_js_reads_the_archives_other_parts() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let part = |base: u64, n: usize, year: u32| -> String {
        let rows: Vec<Value> = (0..n)
            .map(|i| {
                let mut t = tweet(base + i as u64, &post(i), json!({}));
                t["tweet"]["created_at"] = json!(format!("Wed Oct {:02} 20:19:24 +0000 {year}", 1 + i % 28));
                t
            })
            .collect();
        ytd("tweets", &Value::Array(rows))
    };
    std::fs::write(data.join("tweets.js"), part(1000, 21, 2024)).unwrap();
    std::fs::write(data.join("tweets-part1.js"), part(2000, 30, 2019)).unwrap();
    std::fs::write(data.join("tweets-part2.js"), part(3000, 30, 2016)).unwrap();
    let p = build(&tmp.path().join("store"), &data.join("tweets.js"), "t", &[]);
    assert_eq!(items(&p).len(), 81, "tweets.js alone must not drop tweets-part1/2.js");
    let sources: Vec<&str> = p["sources"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(sources, ["2016-Q4", "2019-Q4", "2024-Q4"]);
}

#[test]
fn long_xmaster_posts_stay_social_with_their_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..55).map(|j| format!("word{j}")).collect::<Vec<_>>().join(" ");
    let rows: Vec<Value> = (1..41)
        .map(|i| {
            let pre = if i % 3 == 0 { "@pal @friend " } else { "" };
            xm_row(i, "@longposter", &format!("{pre}{body} and this is long post number {i} with more words at the end of it."), 100)
        })
        .collect();
    let f = tmp.path().join("xm.json");
    std::fs::write(&f, json!({"status": "success", "data": {"tweets": rows}}).to_string()).unwrap();
    let p = build(&tmp.path().join("store"), &f, "lp", &[]);
    assert_eq!(p["register"], "social");
    let it = items(&p);
    assert_eq!(it.len(), 40);
    assert_eq!(count(&it, "reply"), 13);
    assert!(it.iter().all(|i| i.2.get("date").is_some()), "items keep their meta");
}
