//! Tests for the schema writer.
//!
//! The expected texts of [`MEASURED`] are the output of upstream's
//! `create_raw_output_schema` for the same questions, measured with pydantic
//! 2.13.5 and pydantic-core 2.46.5 on CPython 3.14.6; none is derived from the
//! writer under test.

use serde_json::Value;

use super::*;

/// The names upstream's schema tests use as question ids and as labels: the
/// keywords the schema drops, pydantic's own attribute names, and the internal
/// field names of upstream's models.
const FIELD_NAMES: [&str; 12] = [
    "title",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "model_dump",
    "model_config",
    "_private",
    "",
    "with spaces",
    "answer_0",
    "probability_0",
];

/// The keywords upstream removes from every schema object.
const SCHEMA_KEYWORDS: [&str; 5] =
    ["title", "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"];

/// A question set as the SDK's JSON, and the schema upstream writes for it.
struct Measured {
    name: &'static str,
    mode: AnswerMode,
    questions: &'static str,
    schema: &'static str,
}

/// Upstream's schema text for question sets the recorded cassettes do not hold.
const MEASURED: &[Measured] = &[
    Measured {
        name: "eleven_maps",
        mode: AnswerMode::Probabilities,
        questions: "{\"q0\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"x\":null,\"y\":null}},\"q1\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"q2\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"x\":null,\"y\":null}},\"q3\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"q4\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"x\":null,\"y\":null}},\"q5\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"q6\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"x\":null,\"y\":null}},\"q7\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"q8\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"x\":null,\"y\":null}},\"q9\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"q10\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"x\":null,\"y\":null}},\"q11\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]}}",
        schema: "{\"$defs\":{\"ProbabilityMap0\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap1\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap10\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap11\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap2\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap3\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap4\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap5\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap6\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap7\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap8\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap9\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"q0\":{\"$ref\":\"#/$defs/ProbabilityMap0\"},\"q1\":{\"$ref\":\"#/$defs/ProbabilityMap1\"},\"q2\":{\"$ref\":\"#/$defs/ProbabilityMap2\"},\"q3\":{\"$ref\":\"#/$defs/ProbabilityMap3\"},\"q4\":{\"$ref\":\"#/$defs/ProbabilityMap4\"},\"q5\":{\"$ref\":\"#/$defs/ProbabilityMap5\"},\"q6\":{\"$ref\":\"#/$defs/ProbabilityMap6\"},\"q7\":{\"$ref\":\"#/$defs/ProbabilityMap7\"},\"q8\":{\"$ref\":\"#/$defs/ProbabilityMap8\"},\"q9\":{\"$ref\":\"#/$defs/ProbabilityMap9\"},\"q10\":{\"$ref\":\"#/$defs/ProbabilityMap10\"},\"q11\":{\"$ref\":\"#/$defs/ProbabilityMap11\"}},\"required\":[\"q0\",\"q1\",\"q2\",\"q3\",\"q4\",\"q5\",\"q6\",\"q7\",\"q8\",\"q9\",\"q10\",\"q11\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "non_string_content",
        mode: AnswerMode::Probabilities,
        questions: "{\"n\":{\"type\":\"noul\",\"instructions\":{\"b\":1,\"a\":[1.5,\"x<y\",null,true]},\"criteria\":{\"true\":[\"t\",1],\"false\":null}},\"c\":{\"type\":\"choice\",\"instructions\":[\"i\",{\"k\":\"v\"}],\"criteria\":{\"x\":{\"z\":1,\"a\":2},\"y\":[1e+16,0.1]}},\"s\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[{\"lvl\":0},[\"one\"]]}}",
        schema: "{\"$defs\":{\"ProbabilityMap1\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: [\\\"i\\\",{\\\"k\\\":\\\"v\\\"}]\",\"properties\":{\"x\":{\"description\":\"{\\\"z\\\":1,\\\"a\\\":2}\",\"type\":\"number\"},\"y\":{\"description\":\"[1e+16,0.1]\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap2\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"{\\\"lvl\\\":0}\",\"type\":\"number\"},\"1\":{\"description\":\"[\\\"one\\\"]\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"n\":{\"description\":\"Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true.\\nQuestion: {\\\"b\\\":1,\\\"a\\\":[1.5,\\\"x<y\\\",null,true]}\\nTrue criteria: [\\\"t\\\",1]\\nFalse criteria: No additional instructions.\",\"type\":\"number\"},\"c\":{\"$ref\":\"#/$defs/ProbabilityMap1\"},\"s\":{\"$ref\":\"#/$defs/ProbabilityMap2\"}},\"required\":[\"n\",\"c\",\"s\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "non_string_content",
        mode: AnswerMode::Discrete,
        questions: "{\"n\":{\"type\":\"noul\",\"instructions\":{\"b\":1,\"a\":[1.5,\"x<y\",null,true]},\"criteria\":{\"true\":[\"t\",1],\"false\":null}},\"c\":{\"type\":\"choice\",\"instructions\":[\"i\",{\"k\":\"v\"}],\"criteria\":{\"x\":{\"z\":1,\"a\":2},\"y\":[1e+16,0.1]}},\"s\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[{\"lvl\":0},[\"one\"]]}}",
        schema: "{\"$defs\":{\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"n\":{\"description\":\"{\\\"b\\\":1,\\\"a\\\":[1.5,\\\"x<y\\\",null,true]}\\nTrue criteria: [\\\"t\\\",1]\\nFalse criteria: No additional instructions.\",\"type\":\"boolean\"},\"c\":{\"description\":\"[\\\"i\\\",{\\\"k\\\":\\\"v\\\"}]\\nChoice labels, answer with one label:\\nx = {\\\"z\\\":1,\\\"a\\\":2}\\ny = [1e+16,0.1]\",\"enum\":[\"x\",\"y\"],\"type\":\"string\"},\"s\":{\"description\":\"No additional instructions.\\nScore levels, answer with the integer:\\n0 = {\\\"lvl\\\":0}\\n1 = [\\\"one\\\"]\",\"type\":\"integer\"}},\"required\":[\"n\",\"c\",\"s\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "docstring",
        mode: AnswerMode::Probabilities,
        questions: "{\"n\":{\"type\":\"noul\",\"instructions\":\"\\ta\\tb \\n\\n  c\\n\\n\",\"criteria\":null},\"c\":{\"type\":\"choice\",\"instructions\":\"  lead\\ta\\tb \\n\\n    c\\r\\td\\n\\n\",\"criteria\":{\"x\\ty\":\" d\\t1\\n\",\"z\":null}},\"s\":{\"type\":\"score\",\"instructions\":\"\\n\\n\",\"criteria\":[\"\\tlow\\n\",\"high\"]},\"tail\":{\"type\":\"choice\",\"instructions\":\"a\\n  \\n \\t\\n\",\"criteria\":{\"x\":null,\"y\":null}},\"wide\":{\"type\":\"score\",\"instructions\":\"\\u000b\\f\u{a0}\u{e9}\\tb\\n12345678\\t|\\n1234567\\t|\",\"criteria\":[\"a\",\"b\"]},\"json\":{\"type\":\"choice\",\"instructions\":[\"a\\tb\"],\"criteria\":{\"x\":null,\"y\":null}}}",
        schema: "{\"$defs\":{\"ProbabilityMap1\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion:   lead        a       b \\n\\n    c\\r        d\",\"properties\":{\"x\\ty\":{\"description\":\" d\\t1\\n\",\"type\":\"number\"},\"z\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\\ty\",\"z\"],\"type\":\"object\"},\"ProbabilityMap2\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: \",\"properties\":{\"0\":{\"description\":\"\\tlow\\n\",\"type\":\"number\"},\"1\":{\"description\":\"high\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap3\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: a\\n  \\n        \",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"ProbabilityMap4\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: \\u000b\\f\u{a0}\u{e9}  b\\n12345678        |\\n1234567 |\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"ProbabilityMap5\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: [\\\"a\\\\tb\\\"]\",\"properties\":{\"x\":{\"description\":\"No additional instructions.\",\"type\":\"number\"},\"y\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"x\",\"y\"],\"type\":\"object\"},\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"n\":{\"description\":\"Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true.\\nQuestion: \\ta\\tb \\n\\n  c\\n\\n\",\"type\":\"number\"},\"c\":{\"$ref\":\"#/$defs/ProbabilityMap1\"},\"s\":{\"$ref\":\"#/$defs/ProbabilityMap2\"},\"tail\":{\"$ref\":\"#/$defs/ProbabilityMap3\"},\"wide\":{\"$ref\":\"#/$defs/ProbabilityMap4\"},\"json\":{\"$ref\":\"#/$defs/ProbabilityMap5\"}},\"required\":[\"n\",\"c\",\"s\",\"tail\",\"wide\",\"json\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "docstring",
        mode: AnswerMode::Discrete,
        questions: "{\"n\":{\"type\":\"noul\",\"instructions\":\"\\ta\\tb \\n\\n  c\\n\\n\",\"criteria\":{\"true\":\"\\tt\\n\"}},\"c\":{\"type\":\"choice\",\"instructions\":\"  lead\\ta\\tb \\n\\n    c\\r\\td\\n\\n\",\"criteria\":{\"x\\ty\":\" d\\t1\\n\",\"z\":null}},\"s\":{\"type\":\"score\",\"instructions\":\"\\n\\n\",\"criteria\":[\"\\tlow\\n\",\"high\"]}}",
        schema: "{\"$defs\":{\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"n\":{\"description\":\"\\ta\\tb \\n\\n  c\\n\\n\\nTrue criteria: \\tt\\n\\nFalse criteria: No additional instructions.\",\"type\":\"boolean\"},\"c\":{\"description\":\"  lead\\ta\\tb \\n\\n    c\\r\\td\\n\\n\\nChoice labels, answer with one label:\\nx\\ty =  d\\t1\\n\\nz = No additional instructions.\",\"enum\":[\"x\\ty\",\"z\"],\"type\":\"string\"},\"s\":{\"description\":\"\\n\\n\\nScore levels, answer with the integer:\\n0 = \\tlow\\n\\n1 = high\",\"type\":\"integer\"}},\"required\":[\"n\",\"c\",\"s\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "unsorted_names",
        mode: AnswerMode::Probabilities,
        questions: "{\"properties\":{\"type\":\"noul\",\"instructions\":\"p\",\"criteria\":null},\"default\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"properties\":\"lp\",\"default\":\"ld\",\"title\":null}},\"$defs\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"title\":{\"type\":\"noul\",\"instructions\":null,\"criteria\":null}}",
        schema: "{\"$defs\":{\"ProbabilityMap1\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: No additional instructions.\",\"properties\":{\"properties\":{\"type\":\"number\",\"description\":\"lp\"},\"default\":{\"type\":\"number\",\"description\":\"ld\"},\"title\":{\"description\":\"No additional instructions.\",\"type\":\"number\"}},\"required\":[\"properties\",\"default\",\"title\"],\"type\":\"object\"},\"ProbabilityMap2\":{\"additionalProperties\":false,\"description\":\"Each property maps a rubric level to the probability that the document matches it.\\nQuestion: No additional instructions.\",\"properties\":{\"0\":{\"description\":\"a\",\"type\":\"number\"},\"1\":{\"description\":\"b\",\"type\":\"number\"}},\"required\":[\"0\",\"1\"],\"type\":\"object\"},\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"properties\":{\"type\":\"number\",\"description\":\"Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true.\\nQuestion: p\"},\"default\":{\"$ref\":\"#/$defs/ProbabilityMap1\"},\"$defs\":{\"$ref\":\"#/$defs/ProbabilityMap2\"},\"title\":{\"description\":\"Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true.\\nQuestion: No additional instructions.\",\"type\":\"number\"}},\"required\":[\"properties\",\"default\",\"$defs\",\"title\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "unsorted_names",
        mode: AnswerMode::Discrete,
        questions: "{\"properties\":{\"type\":\"noul\",\"instructions\":\"p\",\"criteria\":{}},\"default\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"properties\":\"lp\",\"default\":\"ld\"}},\"$defs\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"title\":{\"type\":\"choice\",\"instructions\":null,\"criteria\":{\"default\":null,\"title\":null}}}",
        schema: "{\"$defs\":{\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"properties\":{\"type\":\"boolean\",\"description\":\"p\\nTrue criteria: No additional instructions.\\nFalse criteria: No additional instructions.\"},\"default\":{\"enum\":[\"properties\",\"default\"],\"type\":\"string\",\"description\":\"No additional instructions.\\nChoice labels, answer with one label:\\nproperties = lp\\ndefault = ld\"},\"$defs\":{\"description\":\"No additional instructions.\\nScore levels, answer with the integer:\\n0 = a\\n1 = b\",\"type\":\"integer\"},\"title\":{\"description\":\"No additional instructions.\\nChoice labels, answer with one label:\\ndefault = No additional instructions.\\ntitle = No additional instructions.\",\"enum\":[\"default\",\"title\"],\"type\":\"string\"}},\"required\":[\"properties\",\"default\",\"$defs\",\"title\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "unsorted_scores",
        mode: AnswerMode::Discrete,
        questions: "{\"default\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]},\"properties\":{\"type\":\"score\",\"instructions\":null,\"criteria\":[\"a\",\"b\"]}}",
        schema: "{\"$defs\":{\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"default\":{\"type\":\"integer\",\"description\":\"No additional instructions.\\nScore levels, answer with the integer:\\n0 = a\\n1 = b\"},\"properties\":{\"type\":\"integer\",\"description\":\"No additional instructions.\\nScore levels, answer with the integer:\\n0 = a\\n1 = b\"}},\"required\":[\"default\",\"properties\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "empty_and_escaped",
        mode: AnswerMode::Probabilities,
        questions: "{\"\":{\"type\":\"noul\",\"instructions\":\"\",\"criteria\":{\"false\":\"f\"}},\"\u{e9}\u{2028}\u{7f}/\\\"\\\\\\u0001\\n\":{\"type\":\"noul\",\"instructions\":\"\u{e9}\u{2028}\u{7f}/\\\"\\\\\\u0001\\u001f\\b\\f\\r\",\"criteria\":null},\"empty_c\":{\"type\":\"choice\",\"instructions\":\"\",\"criteria\":{\"\":\"\",\"b\":\"x\"}}}",
        schema: "{\"$defs\":{\"ProbabilityMap2\":{\"additionalProperties\":false,\"description\":\"Each property maps an option to the probability that it is the best answer.\\nQuestion: \",\"properties\":{\"\":{\"description\":\"\",\"type\":\"number\"},\"b\":{\"description\":\"x\",\"type\":\"number\"}},\"required\":[\"\",\"b\"],\"type\":\"object\"},\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"\":{\"description\":\"Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true.\\nQuestion: \\nTrue criteria: No additional instructions.\\nFalse criteria: f\",\"type\":\"number\"},\"\u{e9}\u{2028}\u{7f}/\\\"\\\\\\u0001\\n\":{\"description\":\"Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true.\\nQuestion: \u{e9}\u{2028}\u{7f}/\\\"\\\\\\u0001\\u001f\\b\\f\\r\",\"type\":\"number\"},\"empty_c\":{\"$ref\":\"#/$defs/ProbabilityMap2\"}},\"required\":[\"\",\"\u{e9}\u{2028}\u{7f}/\\\"\\\\\\u0001\\n\",\"empty_c\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
    Measured {
        name: "empty_and_escaped",
        mode: AnswerMode::Discrete,
        questions: "{\"\":{\"type\":\"noul\",\"instructions\":\"\",\"criteria\":{\"false\":\"f\"}},\"empty_c\":{\"type\":\"choice\",\"instructions\":\"\",\"criteria\":{\"\":\"\",\"b\":\"x\"}},\"s\":{\"type\":\"score\",\"instructions\":\"\",\"criteria\":[\"\",\"b\"]},\"bare\":{\"type\":\"noul\",\"instructions\":null,\"criteria\":null},\"null\":{\"type\":\"noul\",\"instructions\":\"i\",\"criteria\":null}}",
        schema: "{\"$defs\":{\"TypeSafeAnswers\":{\"additionalProperties\":false,\"description\":\"Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.\",\"properties\":{\"\":{\"description\":\"\\nTrue criteria: No additional instructions.\\nFalse criteria: f\",\"type\":\"boolean\"},\"empty_c\":{\"description\":\"\\nChoice labels, answer with one label:\\n = \\nb = x\",\"enum\":[\"\",\"b\"],\"type\":\"string\"},\"s\":{\"description\":\"\\nScore levels, answer with the integer:\\n0 = \\n1 = b\",\"type\":\"integer\"},\"bare\":{\"description\":\"No additional instructions.\",\"type\":\"boolean\"},\"null\":{\"description\":\"i\",\"type\":\"boolean\"}},\"required\":[\"\",\"empty_c\",\"s\",\"bare\",\"null\"],\"type\":\"object\"}},\"additionalProperties\":false,\"properties\":{\"answers\":{\"$ref\":\"#/$defs/TypeSafeAnswers\"}},\"required\":[\"answers\"],\"type\":\"object\"}",
    },
];

/// The writer's text for the question set `questions`, which must be valid.
fn written(questions: &str, mode: AnswerMode) -> String {
    let questions = QuestionModel::from_json(questions).expect("the question set is valid");
    write(&questions, mode).as_str().to_owned()
}

/// Compares the writer's text with upstream's for the measured case `name`
/// in `mode`.
fn assert_measured(name: &str, mode: AnswerMode) {
    let case = MEASURED
        .iter()
        .find(|case| case.name == name && case.mode == mode)
        .expect("the measured case exists");

    assert_eq!(written(case.questions, mode), case.schema, "{name} in {mode:?}");
}

/// The JSON text of an object with `members`, in their order.
fn object(members: impl IntoIterator<Item = (String, String)>) -> String {
    let members = members
        .into_iter()
        .map(|(name, value)| format!("{}:{value}", Value::String(name)))
        .collect::<Vec<_>>();
    format!("{{{}}}", members.join(","))
}

/// The member `name` of the JSON object `value`.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or_else(|| panic!("a member {name:?} in {value}"))
}

/// The names of the members of the JSON object `value`, sorted.
fn sorted_names(value: &Value) -> Vec<&str> {
    let mut names =
        value.as_object().expect("an object").keys().map(String::as_str).collect::<Vec<_>>();
    names.sort_unstable();
    names
}

/// The strings of the JSON array `value`, in its order.
fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|name| name.as_str().expect("a string"))
        .collect()
}

#[test]
fn defs_sort_as_strings_with_eleven_or_more_maps() {
    assert_measured("eleven_maps", AnswerMode::Probabilities);

    let case = MEASURED.iter().find(|case| case.name == "eleven_maps").expect("the case exists");
    let text = written(case.questions, case.mode);
    let definitions = text
        .match_indices("\"ProbabilityMap")
        .filter(|(at, _)| !text[..*at].ends_with("#/$defs/") && !text[..*at].ends_with('/'))
        .map(|(at, found)| {
            let digits = &text[at + found.len()..];
            &digits[..digits.find('"').expect("the closing quote of the name")]
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions, ["0", "1", "10", "11", "2", "3", "4", "5", "6", "7", "8", "9"]);
    assert!(text.contains(r##""q10":{"$ref":"#/$defs/ProbabilityMap10"}"##), "{text}");
}

#[test]
fn non_string_content_is_copied_as_its_compact_json() {
    assert_measured("non_string_content", AnswerMode::Probabilities);
    assert_measured("non_string_content", AnswerMode::Discrete);
}

#[test]
fn non_string_content_loses_the_white_space_between_its_tokens_only() {
    let questions = r#"{"n":{"type":"noul","instructions":{ "b" : [ 1 , "x  y" ] ,
        "a" : null }}}"#;

    let text = written(questions, AnswerMode::Discrete);

    assert!(text.contains(r#""description":"{\"b\":[1,\"x  y\"],\"a\":null}""#), "{text}");
}

#[test]
fn map_descriptions_are_cleaned_as_python_docstrings() {
    assert_measured("docstring", AnswerMode::Probabilities);
}

#[test]
fn descriptions_written_in_place_keep_their_tabs_and_line_ends() {
    assert_measured("docstring", AnswerMode::Discrete);
}

#[test]
fn clean_docstring_expands_tabs_and_drops_the_empty_last_lines() {
    let cases = [
        ("no tab", "a b", "a b"),
        ("a tab at column 0", "\tx", "        x"),
        ("a tab at column 7", "1234567\tx", "1234567 x"),
        ("a tab at column 8", "12345678\tx", "12345678        x"),
        ("the column restarts after a line feed", "123\n\tx", "123\n        x"),
        ("the column restarts after a carriage return", "123\r1\tx", "123\r1       x"),
        (
            "a character is one column whatever its width",
            "\u{e9}\u{3042}\tx",
            "\u{e9}\u{3042}      x",
        ),
        ("empty last lines go", "a\n\n\n", "a"),
        ("an empty line inside stays", "a\n\nb\n", "a\n\nb"),
        ("a last line of spaces stays", "a\n \n", "a\n "),
        ("a last line holding a tab stays as spaces", "a\n\t", "a\n        "),
        ("a last carriage return stays", "a\r\n", "a\r"),
    ];

    for (case, text, expected) in cases {
        assert_eq!(clean_docstring(text), expected, "{case}");
    }
}

#[test]
fn properties_named_properties_or_default_keep_their_generated_order() {
    assert_measured("unsorted_names", AnswerMode::Probabilities);
    assert_measured("unsorted_names", AnswerMode::Discrete);
    assert_measured("unsorted_scores", AnswerMode::Discrete);
}

#[test]
fn empty_names_and_escaped_characters_are_written_as_upstream_writes_them() {
    assert_measured("empty_and_escaped", AnswerMode::Probabilities);
    assert_measured("empty_and_escaped", AnswerMode::Discrete);
}

#[test]
fn every_measured_case_is_named_by_a_test() {
    let named = [
        "eleven_maps",
        "non_string_content",
        "docstring",
        "unsorted_names",
        "unsorted_scores",
        "empty_and_escaped",
    ];

    for case in MEASURED {
        assert!(named.contains(&case.name), "{} is compared by no test", case.name);
    }
}

#[test]
// Upstream: tests/test_schema.py::test_question_ids_preserve_arbitrary_names
fn question_ids_preserve_arbitrary_names() {
    let questions = object(FIELD_NAMES.map(|key| {
        let instructions = Value::String(format!("Evaluate {key}."));
        (key.to_owned(), format!(r#"{{"type":"noul","instructions":{instructions}}}"#))
    }));

    for mode in [AnswerMode::Probabilities, AnswerMode::Discrete] {
        let schema: Value =
            serde_json::from_str(&written(&questions, mode)).expect("the schema is JSON");
        let answers = member(member(&schema, "$defs"), "TypeSafeAnswers");
        let properties = member(answers, "properties");

        assert_eq!(
            member(member(&schema, "properties"), "answers"),
            &serde_json::json!({"$ref": "#/$defs/TypeSafeAnswers"}),
            "{mode:?}"
        );
        let description = member(answers, "description").as_str().expect("a string");
        assert!(description.contains("Use these property names verbatim"), "{mode:?}");
        let mut expected = FIELD_NAMES.to_vec();
        assert_eq!(strings(member(answers, "required")), expected, "{mode:?}");
        expected.sort_unstable();
        assert_eq!(sorted_names(properties), expected, "{mode:?}");
        for key in FIELD_NAMES {
            let answer = member(properties, key);
            let description = member(answer, "description").as_str().expect("a string");
            assert!(description.contains(&format!("Evaluate {key}.")), "{mode:?}: {key:?}");
            for keyword in SCHEMA_KEYWORDS {
                assert!(answer.get(keyword).is_none(), "{mode:?}: {key:?} has {keyword}");
            }
        }
    }
}

#[test]
// Upstream: tests/test_schema.py::test_probability_labels_preserve_arbitrary_names
fn probability_labels_preserve_arbitrary_names() {
    let criteria = object(
        FIELD_NAMES
            .map(|key| (key.to_owned(), Value::String(format!("The {key} option.")).to_string())),
    );
    let questions = format!(r#"{{"level":{{"type":"choice","criteria":{criteria}}}}}"#);

    let schema: Value = serde_json::from_str(&written(&questions, AnswerMode::Probabilities))
        .expect("the schema is JSON");
    let probabilities = member(member(&schema, "$defs"), "ProbabilityMap0");
    let properties = member(probabilities, "properties");

    let mut expected = FIELD_NAMES.to_vec();
    assert_eq!(strings(member(probabilities, "required")), expected);
    expected.sort_unstable();
    assert_eq!(sorted_names(properties), expected);
    assert!(probabilities.get("title").is_none());
    for key in FIELD_NAMES {
        let probability = member(properties, key);
        assert_eq!(member(probability, "description"), &format!("The {key} option."), "{key:?}");
        for keyword in SCHEMA_KEYWORDS {
            assert!(probability.get(keyword).is_none(), "{key:?} has {keyword}");
        }
    }
}
