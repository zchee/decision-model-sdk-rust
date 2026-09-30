# Response fixtures

Response bodies taken from the upstream Python SDK's tests (`typesafe-ai/typesafe-sdk-python`, tag `v0.7.2`, commit
`f078f1e`), written as the compact JSON its test transport sends.

| File | Upstream source |
| --- | --- |
| `result.json` | `RESULT`, `tests/test_clients.py:42-56`, used by `test_round_trip` and most of `tests/test_responses.py` |
| `unknown-answer-type.json` | the response body of `test_unknown_answer_type_ignored`, `tests/test_responses.py` |
| `unknown-extra-fields.json` | the response body of `test_unknown_extra_fields_tolerated`, `tests/test_responses.py` |
| `structured-legend.json` | the response body of `test_extra_body_shallow_override`, `tests/test_clients.py` |
| `empty-answers.json` | the response body of `test_array_inputs`, `tests/test_types.py` (also `test_raw_optional_fields_preserve_explicit_null` and `test_explicitly_nullable_json_values`) |
| `models.json` | `{"models": [CARD]}` of `test_models_shape`, `tests/test_clients.py` (`CARD` is at line 57) |
| `models-extra-fields.json` | the card of `test_models_ignore_unknown_fields`, `tests/test_clients.py` |
