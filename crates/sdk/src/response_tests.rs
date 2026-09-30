use http::HeaderValue;
use serde_json::json;

use super::*;
use crate::{
    codec,
    de::{AnswerContext, decode_system_one_with},
};

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("../tests/fixtures/result.json");

fn decode(body: &[u8]) -> SystemOneResponse {
    let mut headers = HeaderMap::new();
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req-export"));
    decode_system_one_with(
        Bytes::copy_from_slice(body),
        StatusCode::OK,
        headers,
        AnswerContext::new(3),
        None,
    )
    .expect("the body decodes")
}

fn through_the_codec<T: Serialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    codec::encode_into(&mut out, value).expect("the value encodes");
    out
}

/// `test_response_serialization_excludes_http_metadata`, the System One half.
#[test]
fn serializing_a_response_writes_the_payload_and_not_the_http_metadata() {
    let response = decode(RESULT);
    let body: serde_json::Value = serde_json::from_slice(RESULT).expect("the fixture parses");
    // Iterating the typed views first must not change what is serialized.
    assert_eq!(response.answers().choices().count(), 1);
    assert_eq!(response.answers().scores().count(), 1);

    let through_serde_json = serde_json::to_value(&response).expect("the response serializes");
    assert_eq!(through_serde_json, body);
    let keys: Vec<&String> =
        through_serde_json.as_object().expect("a response is an object").keys().collect();
    assert_eq!(keys, ["answers", "model", "usage"]);

    let encoded = through_the_codec(&response);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&encoded).expect("JSON"), body);
    assert_eq!(
        std::str::from_utf8(&encoded).expect("UTF-8"),
        std::str::from_utf8(RESULT).expect("UTF-8").trim_end(),
        "this codec writes the fixture back byte for byte"
    );

    let restored = decode(&encoded);
    assert_eq!(restored.model(), response.model());
    assert_eq!(restored.usage(), response.usage());
    assert_eq!(restored.answers(), response.answers());
    assert_eq!(restored.meta().request_id(), Some("req-export"));
    assert_eq!(&response.meta().raw_body()[..], RESULT);
}

/// `test_answer_attributes_and_dictionary_types`.
#[test]
fn each_answer_serializes_with_its_type() {
    let noul = NoulAnswer::new(0.98);
    let choice = ChoiceAnswer::new("billing", 0.9, [("billing", 0.9), ("support", 0.1)]);

    assert_eq!(noul.noul(), 0.98);
    assert_eq!(choice.choice(), "billing");
    assert_eq!(choice.confidence(), 0.9);
    assert_eq!(choice.probabilities().collect::<Vec<_>>(), [("billing", 0.9), ("support", 0.1)]);
    assert_eq!(
        serde_json::to_value(noul).expect("serializes"),
        json!({"type": "noul", "noul": 0.98})
    );
    assert_eq!(
        serde_json::to_value(&choice).expect("serializes"),
        json!({"type": "choice", "choice": "billing", "confidence": 0.9, "probabilities": {"billing": 0.9, "support": 0.1}})
    );
    assert_eq!(
        String::from_utf8(through_the_codec(&choice)).expect("UTF-8"),
        r#"{"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.9,"support":0.1}}"#
    );
}

/// `test_response_preserves_nested_json`.
#[test]
fn a_structured_legend_description_serializes_as_data() {
    let description =
        Content::json(&json!({"examples": ["a", {"note": null}]})).expect("an object is content");
    let answer = ScoreAnswer::new(0.0, 1.0, [(0, description.clone())], [(0, 1.0)]);

    assert_eq!(answer.description(0), Some(&description));
    assert_eq!(answer.probability(0), Some(1.0));
    let exported = json!({
        "type": "score",
        "score": 0.0,
        "confidence": 1.0,
        "legend": {"0": {"examples": ["a", {"note": null}]}},
        "probabilities": {"0": 1.0},
    });
    assert_eq!(serde_json::to_value(&answer).expect("serializes through serde_json"), exported);
    let encoded = through_the_codec(&answer);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&encoded).expect("JSON"), exported);
    assert_eq!(
        String::from_utf8(encoded).expect("UTF-8"),
        r#"{"type":"score","score":0.0,"confidence":1.0,"legend":{"0":{"examples":["a",{"note":null}]}},"probabilities":{"0":1.0}}"#
    );

    let result = Answers::from_iter([("q", Answer::from(answer.clone()))]);
    assert_eq!(result.choices().count(), 0);
    assert_eq!(result.score("q"), Some(&answer));
}

#[test]
fn a_score_built_by_hand_is_sorted_by_level() {
    let answer = ScoreAnswer::new(
        1.5,
        0.5,
        [(2, Content::text("high")), (0, Content::text("low")), (1, Content::text("mid"))],
        [(1, 0.5), (2, 0.25), (0, 0.25)],
    );

    assert_eq!(answer.score(), 1.5);
    assert_eq!(answer.confidence(), 0.5);
    let legend: Vec<(u32, Option<&str>)> =
        answer.legend().map(|(level, description)| (level, description.as_text())).collect();
    assert_eq!(legend, [(0, Some("low")), (1, Some("mid")), (2, Some("high"))]);
    assert_eq!(answer.probabilities().collect::<Vec<_>>(), [(0, 0.25), (1, 0.5), (2, 0.25)]);
    assert_eq!(answer.probabilities().next_back(), Some((2, 0.25)));
    assert_eq!(answer.description(3), None);
    assert_eq!(answer.probability(5), None);
}

/// The sorted insertion the level lists were built with before they were
/// sorted once at the end: the reference the new order is held to.
fn inserted_by_level<T>(levels: impl IntoIterator<Item = (u32, T)>) -> Vec<(u32, T)> {
    let mut entries = Vec::new();
    for (level, value) in levels {
        let at = entries.partition_point(|(key, _): &(u32, T)| *key <= level);
        entries.insert(at, (level, value));
    }
    entries
}

/// Level sequences in every order a sender can choose, each level tagged with
/// its position on the wire so that the order of equal levels is visible.
fn level_orders() -> Vec<(&'static str, Vec<(u32, usize)>)> {
    let tagged =
        |levels: Vec<u32>| levels.into_iter().enumerate().map(|(at, level)| (level, at)).collect();
    // A fixed linear congruential sequence: a shuffle that is the same on
    // every run, so a failure reproduces.
    let mut state = 0x2545_f491_u32;
    let shuffled: Vec<u32> = (0..257)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 16) % 64
        })
        .collect();
    vec![
        ("empty", Vec::new()),
        ("one", tagged(vec![7])),
        ("ascending", tagged((0..300).collect())),
        ("descending", tagged((0..300).rev().collect())),
        ("shuffled with repeats", tagged(shuffled)),
        ("all equal", tagged(vec![4; 40])),
        ("repeats out of order", tagged(vec![3, 1, 3, 0, 1, 3, 0])),
        ("ascending then one early", tagged((1..50).chain([0]).collect())),
        ("the extremes", tagged(vec![u32::MAX, 0, u32::MAX, 0])),
    ]
}

/// A wire position as a probability, so that it survives the trip.
fn tag(at: usize) -> f64 {
    f64::from(u32::try_from(at).expect("a test list is short"))
}

/// What the decoders do with a level list: append, then sort once if a
/// level arrived out of order.
fn pushed_by_level<T>(levels: impl IntoIterator<Item = (u32, T)>) -> Vec<(u32, T)> {
    let mut entries = Vec::new();
    let mut in_order = true;
    for (level, value) in levels {
        push_by_level(&mut entries, &mut in_order, level, value);
    }
    assert_eq!(
        in_order,
        entries.is_sorted_by_key(|(level, _)| *level),
        "in_order says whether the appended list is already sorted"
    );
    if !in_order {
        sort_by_level(&mut entries);
    }
    entries
}

#[test]
fn a_level_list_sorted_once_equals_the_list_built_by_sorted_insertion() {
    for (order, levels) in level_orders() {
        let expected = inserted_by_level(levels.iter().copied());

        assert_eq!(pushed_by_level(levels.iter().copied()), expected, "levels {order}: {levels:?}");

        let answer = ScoreAnswer::new(
            0.0,
            0.0,
            levels.iter().map(|&(level, at)| (level, Content::text(at.to_string()))),
            levels.iter().map(|&(level, at)| (level, tag(at))),
        );
        let legend: Vec<(u32, usize)> = answer
            .legend()
            .map(|(level, description)| {
                let tag = description.as_text().expect("a text description");
                (level, tag.parse().expect("the tag is a number"))
            })
            .collect();
        assert_eq!(legend, expected, "ScoreAnswer::new legend, levels {order}");
        let tagged: Vec<(u32, f64)> =
            expected.iter().map(|&(level, at)| (level, tag(at))).collect();
        assert_eq!(
            answer.probabilities().collect::<Vec<_>>(),
            tagged,
            "ScoreAnswer::new probabilities, levels {order}"
        );
    }
}

/// A response with one score whose legend and probabilities list `levels`
/// in the order given, each value naming its wire position.
fn score_body(levels: &[(u32, usize)]) -> String {
    let legend: Vec<String> =
        levels.iter().map(|(level, at)| format!(r#""{level}":"{at}""#)).collect();
    let probabilities: Vec<String> =
        levels.iter().map(|(level, at)| format!(r#""{level}":{at}"#)).collect();
    format!(
        r#"{{"model":"m","usage":{{}},"answers":{{"s":{{"type":"score","score":1,"confidence":1,"legend":{{{}}},"probabilities":{{{}}}}}}}}}"#,
        legend.join(","),
        probabilities.join(",")
    )
}

#[test]
fn a_decoded_level_list_equals_the_list_built_by_sorted_insertion() {
    for (order, levels) in level_orders() {
        let expected = inserted_by_level(levels.iter().copied());
        let body = score_body(&levels);

        let response = decode(body.as_bytes());

        let score = response.answers().score("s").expect("s is a score answer");
        let legend: Vec<(u32, usize)> = score
            .legend()
            .map(|(level, description)| {
                let tag = description.as_text().expect("a text description");
                (level, tag.parse().expect("the tag is a number"))
            })
            .collect();
        assert_eq!(legend, expected, "decoded legend, levels {order}");
        let tagged: Vec<(u32, f64)> =
            expected.iter().map(|&(level, at)| (level, tag(at))).collect();
        assert_eq!(
            score.probabilities().collect::<Vec<_>>(),
            tagged,
            "decoded probabilities, levels {order}"
        );
    }
}

/// Levels in descending order, the order that made each insert move the
/// whole list.
const DESCENDING_LEVELS: u32 = 200_000;

/// The time [`DESCENDING_LEVELS`] may take to decode in an unoptimized
/// build: about 12 times what appending and sorting once takes on a laptop,
/// so a runner slowed by other jobs or by coverage instrumentation stays
/// under it, and about a fifth of what moving the tail on every insert takes,
/// which grows with the square of the count.
const DESCENDING_LEVELS_BOUND: std::time::Duration = std::time::Duration::from_secs(3);

#[test]
fn levels_in_descending_order_decode_in_n_log_n_time() {
    let levels: Vec<(u32, usize)> = (0..DESCENDING_LEVELS).rev().map(|level| (level, 0)).collect();
    let body = score_body(&levels);

    let started = std::time::Instant::now();
    let response = decode(body.as_bytes());
    let elapsed = started.elapsed();

    println!(
        "{DESCENDING_LEVELS} descending levels in a legend and in the probabilities \
         ({} bytes) decoded in {elapsed:?}",
        body.len()
    );
    let score = response.answers().score("s").expect("s is a score answer");
    assert_eq!(score.legend().len(), levels.len());
    assert!(
        score.probabilities().map(|(level, _)| level).eq(0..DESCENDING_LEVELS),
        "the probabilities are sorted by level"
    );
    assert!(
        elapsed < DESCENDING_LEVELS_BOUND,
        "{DESCENDING_LEVELS} descending levels took {elapsed:?}, over the bound of \
         {DESCENDING_LEVELS_BOUND:?}: the level lists cost more than a sort"
    );
}

#[test]
fn a_repeated_level_keeps_its_entries_in_arrival_order() {
    let entries = pushed_by_level([(3, 'a'), (1, 'b'), (3, 'c'), (0, 'd'), (1, 'e')]);

    assert_eq!(entries, [(0, 'd'), (1, 'b'), (1, 'e'), (3, 'a'), (3, 'c')]);
}

#[test]
fn answers_are_looked_up_by_name_and_filtered_by_kind_without_copying() {
    let answers = Answers::from_iter([
        ("spam", Answer::from(NoulAnswer::new(0.1))),
        ("tone", Answer::from(ChoiceAnswer::new("calm", 0.8, [("calm", 0.8), ("angry", 0.2)]))),
        ("urgency", Answer::from(ScoreAnswer::new(1.0, 0.7, [], [(1, 1.0)]))),
        ("billing", Answer::from(NoulAnswer::new(0.9))),
    ]);

    assert_eq!(answers.len(), 4);
    assert!(!answers.is_empty());
    assert_eq!(answers.names().collect::<Vec<_>>(), ["spam", "tone", "urgency", "billing"]);
    assert_eq!(answers.names().next_back(), Some("billing"));
    assert_eq!(
        answers.nouls().map(|(name, answer)| (name, answer.noul())).collect::<Vec<_>>(),
        [("spam", 0.1), ("billing", 0.9)]
    );
    assert_eq!(answers.choices().map(|(name, _)| name).collect::<Vec<_>>(), ["tone"]);
    assert_eq!(answers.scores().map(|(name, _)| name).collect::<Vec<_>>(), ["urgency"]);
    assert_eq!(answers.iter().len(), 4);

    assert_eq!(answers.noul("billing").map(NoulAnswer::noul), Some(0.9));
    assert_eq!(answers.choice("tone").and_then(|tone| tone.probability("angry")), Some(0.2));
    assert_eq!(answers.choice("tone").and_then(|tone| tone.probability("bored")), None);
    assert_eq!(answers.score("urgency").map(ScoreAnswer::score), Some(1.0));
    // A name of another kind, or no such name, is `None` rather than a panic.
    assert_eq!(answers.noul("tone"), None);
    assert_eq!(answers.choice("urgency"), None);
    assert_eq!(answers.score("spam"), None);
    assert_eq!(answers.get("missing"), None);

    let tone = answers.get("tone").expect("tone is there");
    assert_eq!((tone.as_noul(), tone.as_score()), (None, None));
    let urgency = answers.get("urgency").expect("urgency is there");
    assert_eq!((urgency.as_noul(), urgency.as_choice()), (None, None));
    let spam = answers.get("spam").expect("spam is there");
    assert_eq!((spam.as_choice(), spam.as_score()), (None, None));

    assert!(Answers::default().is_empty());
    assert_eq!(serde_json::to_value(Answers::default()).expect("serializes"), json!({}));
}

#[test]
fn usage_counts_are_optional_and_serialize_as_null_when_absent() {
    let usage = Usage::new(Some(120), None);

    assert_eq!(usage.input_tokens(), Some(120));
    assert_eq!(usage.output_tokens(), None);
    assert_eq!(
        serde_json::to_value(usage).expect("serializes"),
        json!({"input_tokens": 120, "output_tokens": null})
    );
    assert_eq!(Usage::default(), Usage::new(None, None));
}

#[test]
fn the_meta_keeps_the_http_response_and_prints_none_of_its_contents() {
    let mut headers = HeaderMap::new();
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req-7"));
    headers.insert("set-cookie", HeaderValue::from_static("session=secret-cookie"));
    let meta =
        ResponseMeta::new(StatusCode::OK, headers.clone(), Bytes::from_static(b"{\"secret\":1}"));

    assert_eq!(meta.status(), StatusCode::OK);
    assert_eq!(meta.headers(), &headers);
    assert_eq!(meta.request_id(), Some("req-7"));
    assert_eq!(&meta.raw_body()[..], b"{\"secret\":1}");
    let rendered = format!("{meta:?}");
    assert_eq!(
        rendered,
        "ResponseMeta { status: 200, request_id: Some(\"req-7\"), headers: <2 headers>, body: <12 bytes> }"
    );
    assert!(!rendered.contains("secret"), "{rendered}");

    let (status, parts_headers, body) = meta.clone().into_parts();
    assert_eq!(
        (status, &parts_headers, &body[..]),
        (StatusCode::OK, &headers, &b"{\"secret\":1}"[..])
    );
}

#[test]
fn a_request_id_that_is_absent_or_not_text_is_none() {
    let absent = ResponseMeta::new(StatusCode::OK, HeaderMap::new(), Bytes::new());
    assert_eq!(absent.request_id(), None);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-typesafe-request-id",
        HeaderValue::from_bytes(b"req-\xff").expect("an opaque header value is valid"),
    );
    let opaque = ResponseMeta::new(StatusCode::OK, headers, Bytes::new());
    assert_eq!(opaque.request_id(), None);
}

#[test]
fn a_response_gives_up_its_answers_and_compares_whole() {
    let response = decode(RESULT);
    let copy = response.clone();

    assert_eq!(copy, response);
    let answers = copy.into_answers();
    assert_eq!(&answers, response.answers());

    let other = SystemOneResponse::from_parts(
        Name::from(response.model()),
        *response.usage(),
        answers,
        ResponseMeta::new(StatusCode::OK, HeaderMap::new(), response.meta().raw_body().clone()),
    );
    assert_ne!(other, response, "the HTTP metadata is part of equality");
}

/// Names on both sides of the inline limit (24 bytes on a 64-bit target),
/// multi-byte names on both sides of it, and names the body writes with
/// escapes, in every place a response holds a name: the model, the answer
/// names, a choice's pick and its option names.
const NAMES: &str = concat!(
    r#"{"model":"jev-2026-09-19-model-name-longer-than-24-bytes","#,
    r#""usage":{"input_tokens":1,"output_tokens":2},"answers":{"#,
    r#""exactly_twenty_four_byte":{"type":"noul","noul":0.5},"#,
    r#""twenty_five_bytes_exactly":{"type":"noul","noul":0.25},"#,
    "\"\u{8cea}\u{554f}\u{306e}\u{540d}\u{524d}\":{\"type\":\"noul\",\"noul\":0.75},",
    "\"\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{8cea}\u{554f}\u{306e}\u{540d}\":",
    r#"{"type":"choice","choice":"#,
    "\"\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}\",",
    r#""confidence":0.5,"probabilities":{"#,
    "\"\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}\":0.5,",
    r#""tab\there":0.25,"an option called \"quoted\", longer than 24":0.25}},"#,
    r#""tab\tname":{"type":"noul","noul":0.125}}}"#,
);

#[test]
fn long_multi_byte_and_escaped_names_decode_serialize_and_print_as_text() {
    let long_pick =
        "\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}";
    let long_question = "\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{8cea}\u{554f}\u{306e}\u{540d}";
    let short_question = "\u{8cea}\u{554f}\u{306e}\u{540d}\u{524d}";
    assert_eq!((long_pick.len(), long_question.len(), short_question.len()), (33, 27, 15));

    let response = decode(NAMES.as_bytes());

    assert_eq!(response.model(), "jev-2026-09-19-model-name-longer-than-24-bytes");
    assert_eq!(
        response.answers().names().collect::<Vec<_>>(),
        [
            "exactly_twenty_four_byte",
            "twenty_five_bytes_exactly",
            short_question,
            long_question,
            "tab\tname"
        ]
    );
    assert_eq!(
        response.answers().noul("twenty_five_bytes_exactly").map(NoulAnswer::noul),
        Some(0.25)
    );
    assert_eq!(response.answers().noul(short_question).map(NoulAnswer::noul), Some(0.75));
    assert_eq!(response.answers().noul("tab\tname").map(NoulAnswer::noul), Some(0.125));
    let choice = response.answers().choice(long_question).expect("the long name finds its answer");
    assert_eq!(choice.choice(), long_pick);
    assert_eq!(
        choice.probabilities().collect::<Vec<_>>(),
        [
            (long_pick, 0.5),
            ("tab\there", 0.25),
            ("an option called \"quoted\", longer than 24", 0.25)
        ]
    );
    assert_eq!(choice.probability("an option called \"quoted\", longer than 24"), Some(0.25));

    // Written back, the body is the one that came in, byte for byte: the
    // multi-byte names unescaped and the tab and quotes escaped as JSON
    // writes them.
    assert_eq!(std::str::from_utf8(&through_the_codec(&response)).expect("UTF-8"), NAMES);
    let through_serde_json = serde_json::to_string(&response).expect("the response serializes");
    assert_eq!(through_serde_json, NAMES);

    // `Debug` prints each name as a quoted, escaped string, exactly as it
    // printed when the names were `String`s.
    assert_eq!(format!("{choice:?}"), EXPECTED_CHOICE_DEBUG);
    assert_eq!(format!("{:?}", response.answers()), EXPECTED_ANSWERS_DEBUG);
    let rendered = format!("{response:?}");
    assert!(
        rendered.starts_with(
            "SystemOneResponse { model: \"jev-2026-09-19-model-name-longer-than-24-bytes\", usage: "
        ),
        "{rendered}"
    );
}

/// `ChoiceAnswer`'s `Debug` output for `NAMES`' choice, as printed when its
/// names were `String`s.
const EXPECTED_CHOICE_DEBUG: &str = concat!(
    r#"ChoiceAnswer { choice: ""#,
    "\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}",
    r#"", confidence: 0.5, probabilities: [(""#,
    "\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}",
    r#"", 0.5), ("tab\there", 0.25), ("an option called \"quoted\", longer than 24", 0.25)] }"#,
);

/// `Answers`' `Debug` output for `NAMES`, as printed when its names were
/// `String`s.
const EXPECTED_ANSWERS_DEBUG: &str = concat!(
    r#"Answers { entries: [("exactly_twenty_four_byte", Noul(NoulAnswer { noul: 0.5 })), "#,
    r#"("twenty_five_bytes_exactly", Noul(NoulAnswer { noul: 0.25 })), (""#,
    "\u{8cea}\u{554f}\u{306e}\u{540d}\u{524d}",
    r#"", Noul(NoulAnswer { noul: 0.75 })), (""#,
    "\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{8cea}\u{554f}\u{306e}\u{540d}",
    r#"", Choice("#,
    // The choice prints inside the set exactly as it prints alone.
    r#"ChoiceAnswer { choice: ""#,
    "\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}",
    r#"", confidence: 0.5, probabilities: [(""#,
    "\u{9078}\u{629e}\u{80a2}\u{3001}\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{540d}\u{524d}",
    r#"", 0.5), ("tab\there", 0.25), ("an option called \"quoted\", longer than 24", 0.25)] }"#,
    r#")), ("tab\tname", Noul(NoulAnswer { noul: 0.125 }))] }"#,
);
