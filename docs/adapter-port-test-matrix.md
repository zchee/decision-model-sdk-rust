# Upstream test matrix of the adapter

Every test function of the Python adapter's `tests/test_*.py` and `tests/utils/test_*.py`
([system-one-adapter-python](https://github.com/typesafe-ai/system-one-adapter-python) 0.2.1,
commit `e1d4cc9`) and where its behaviour is covered here: a Rust test that exists in this
repository, named `path::function`, or a row of the deviations table in
[`crates/adapter/README.md`](../crates/adapter/README.md#deviations-from-the-python-adapter),
quoted by its first cell. Nothing of this upstream is excluded. The functions and their cases
were collected with `uv run --offline --all-extras --all-groups pytest --collect-only -q` on
upstream at `e1d4cc9`. `.github/scripts/port-test-matrix.py` checks every row; its docstring
lists the checks. The matrix of the Python SDK is [`port-test-matrix.md`](port-test-matrix.md).

A parametrized upstream test is one row, and **Cases** is the number of cases pytest collects
for it. Upstream runs many tests once with the synchronous client or provider and once with the
asynchronous one: each such row maps to the Rust test of the asynchronous client, and the
synchronous half is covered, once for all of them, by the README deviation row "Synchronous
client" (this crate has no blocking client). A function that is ported in part maps to its Rust
tests; the README deviation row that explains the rest names it in its text.

Each Rust test of `crates/adapter` and `crates/adapter-live-tests` that a row below names carries
`// Upstream: <file>::<function>` above its `fn`, naming that row's function, and every such line
in those two crates names a function whose row lists the test below it; the checker holds both
directions. The three SDK-side targets (the two tests in `crates/sdk/src/question_tests.rs` and
`live_questions`) carry no such line. Tests of the adapter that port no upstream function (its
security tests, the tests that pin its defaults, the `key_echo_` tests of a success response that
repeats the key) carry no such line and have no row: the matrix
lists upstream functions, not Rust tests.

## Counts

| Upstream file | Functions | Cases | Rust test | Deviation row |
| --- | ---: | ---: | ---: | ---: |
| `tests/test_client_with_fake_model.py` | 12 | 54 | 12 | 0 |
| `tests/test_client_with_live_apis.py` | 3 | 25 | 3 | 0 |
| `tests/test_gemini_transports.py` | 3 | 14 | 3 | 0 |
| `tests/test_openai_transports.py` | 4 | 23 | 4 | 0 |
| `tests/test_provider_lifecycle.py` | 13 | 68 | 7 | 6 |
| `tests/test_provider_nonanswers.py` | 4 | 136 | 4 | 0 |
| `tests/test_provider_requests.py` | 16 | 29 | 16 | 0 |
| `tests/test_provider_retries.py` | 1 | 12 | 1 | 0 |
| `tests/test_schema.py` | 6 | 24 | 6 | 0 |
| `tests/utils/test_confidence_metrics.py` | 1 | 8 | 1 | 0 |
| `tests/utils/test_error_handling.py` | 7 | 28 | 5 | 2 |
| `tests/utils/test_probability_normalization.py` | 1 | 3 | 1 | 0 |

## `tests/test_client_with_fake_model.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_sdk_questions_and_response_serialization` | 4 | `crates/adapter/tests/client.rs::sdk_questions_and_response_serialization` |
| `test_prompted_mode_adds_schema_instructions_native_does_not` | 2 | `crates/adapter/tests/client.rs::prompted_mode_adds_schema_instructions_native_does_not` |
| `test_structured_state_prompt_is_delimited_and_escapes_embedded_tags` | 1 | `crates/adapter/tests/client.rs::structured_state_prompt_is_delimited_and_escapes_embedded_tags` |
| `test_transient_errors_are_retried` | 4 | `crates/adapter/tests/client.rs::transient_errors_are_retried` |
| `test_retries_are_exhausted` | 2 | `crates/adapter/tests/client.rs::retries_are_exhausted` |
| `test_malformed_retry_exhaustion_preserves_debug` | 8 | `crates/adapter/tests/client.rs::malformed_retry_exhaustion_preserves_debug` |
| `test_usage_totals_preserve_unknown_counts_across_corrections` | 14 | `crates/adapter/tests/client.rs::usage_totals_preserve_unknown_counts_across_corrections` |
| `test_usage_separates_last_attempt_from_cumulative_totals` | 2 | `crates/adapter/tests/client.rs::usage_separates_last_attempt_from_cumulative_totals` |
| `test_attempts_are_independent_and_replayable` | 2 | `crates/adapter/tests/client.rs::attempts_are_independent_and_replayable` |
| `test_invalid_questions_are_rejected` | 5 | `crates/adapter/tests/client.rs::invalid_questions_are_rejected`, `crates/sdk/src/question_tests.rs::an_empty_set_is_rejected`, `crates/sdk/src/question_tests.rs::a_score_without_levels_is_rejected_typed_or_raw` |
| `test_malformed_structure_is_retried` | 8 | `crates/adapter/tests/client.rs::malformed_structure_is_retried` |
| `test_missing_provider_setting_is_rejected` | 2 | `crates/adapter/tests/client.rs::missing_provider_setting_is_rejected` |

## `tests/test_client_with_live_apis.py`

These tests are billed. The rows of the two vendor functions name the tests of
`crates/adapter-live-tests`, twelve for each function; each builds its provider with the key
the test reads and gives the provider to the client with `provider_instance`, where upstream
names the provider and the client reads the key from the environment. The TypeSafe row tests the
TypeSafe API rather than the adapter, so it names the SDK's billed `live_questions`.
Neither CI nor the gate runs a billed test.

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_live_responses_match_reference_shape` | 12 | `crates/adapter-live-tests/tests/live.rs::reference_shape_probabilities_prompted_openai`, `crates/adapter-live-tests/tests/live.rs::reference_shape_probabilities_prompted_anthropic`, `crates/adapter-live-tests/tests/live.rs::reference_shape_probabilities_prompted_gemini`, `crates/adapter-live-tests/tests/live.rs::reference_shape_probabilities_native_openai`, `crates/adapter-live-tests/tests/live.rs::reference_shape_probabilities_native_anthropic`, `crates/adapter-live-tests/tests/live.rs::reference_shape_probabilities_native_gemini`, `crates/adapter-live-tests/tests/live.rs::reference_shape_discrete_prompted_openai`, `crates/adapter-live-tests/tests/live.rs::reference_shape_discrete_prompted_anthropic`, `crates/adapter-live-tests/tests/live.rs::reference_shape_discrete_prompted_gemini`, `crates/adapter-live-tests/tests/live.rs::reference_shape_discrete_native_openai`, `crates/adapter-live-tests/tests/live.rs::reference_shape_discrete_native_anthropic`, `crates/adapter-live-tests/tests/live.rs::reference_shape_discrete_native_gemini` |
| `test_live_models_follow_question_instructions_and_criteria` | 12 | `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_probabilities_prompted_openai`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_probabilities_prompted_anthropic`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_probabilities_prompted_gemini`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_probabilities_native_openai`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_probabilities_native_anthropic`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_probabilities_native_gemini`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_discrete_prompted_openai`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_discrete_prompted_anthropic`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_discrete_prompted_gemini`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_discrete_native_openai`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_discrete_native_anthropic`, `crates/adapter-live-tests/tests/live.rs::follows_instructions_and_criteria_discrete_native_gemini` |
| `test_live_typesafe_response_matches_reference_shape` | 1 | `crates/live-tests/tests/live.rs::live_questions` |

## `tests/test_gemini_transports.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_gemini_transport_preserves_corrections_and_usage` | 4 | `crates/adapter/tests/providers_gemini.rs::gemini_transport_preserves_corrections_and_usage` |
| `test_gemini_incomplete_http_response_is_not_an_answer` | 2 | `crates/adapter/tests/providers_gemini.rs::gemini_incomplete_response_is_not_an_answer` |
| `test_gemini_transport_errors_obey_retry_budget` | 8 | `crates/adapter/tests/providers_gemini.rs::gemini_transport_errors_obey_the_budget` |

## `tests/test_openai_transports.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_openai_transport_preserves_corrections_and_usage` | 16 | `crates/adapter/tests/providers_openai.rs::openai_transport_preserves_corrections_and_usage` |
| `test_unfinished_responses_are_not_treated_as_answers` | 2 | `crates/adapter/src/provider/openai_tests.rs::an_unfinished_response_is_not_an_answer` |
| `test_concurrent_attempts_are_isolated_and_preserve_failed_responses` | 3 | `crates/adapter/tests/providers_openai.rs::concurrent_attempts_are_isolated_when_the_first_completes`, `crates/adapter/tests/providers_openai.rs::concurrent_attempts_are_isolated_when_the_first_is_incomplete`, `crates/adapter/tests/providers_openai.rs::concurrent_attempts_are_isolated_when_the_first_failed` |
| `test_custom_endpoint_from_environment_defaults_to_chat` | 2 | `crates/adapter/src/provider/openai_tests.rs::a_custom_endpoint_from_the_environment_defaults_to_chat` |

## `tests/test_provider_lifecycle.py`

The six functions that close the client map to the README row "Closing the client"; the
close half of `test_reuses_owned_provider_and_closes_sdk_on_context_exit` is named there too.

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_reuses_owned_provider_and_closes_sdk_on_context_exit` | 4 | `crates/adapter/tests/lifecycle.rs::lifecycle_reuses_owned_provider_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_reuses_owned_provider_anthropic` |
| `test_cache_uses_resolved_provider_and_model_and_is_per_client` | 4 | `crates/adapter/tests/lifecycle.rs::lifecycle_cache_key_is_resolved_pair_per_client_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_cache_key_is_resolved_pair_per_client_anthropic` |
| `test_injected_provider_is_borrowed` | 8 | `crates/adapter/tests/lifecycle.rs::lifecycle_injected_on_the_client_is_borrowed_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_injected_per_call_is_borrowed_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_injected_on_the_client_is_borrowed_anthropic`, `crates/adapter/tests/lifecycle.rs::lifecycle_injected_per_call_is_borrowed_anthropic` |
| `test_custom_provider_without_close_remains_supported` | 4 | `crates/adapter/tests/lifecycle.rs::lifecycle_custom_provider_is_supported_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_custom_provider_is_supported_anthropic` |
| `test_exceptional_exit_closes_owned_sdks` | 16 | Deviation: "Closing the client" |
| `test_cleanup_continues_after_failure` | 4 | Deviation: "Closing the client" |
| `test_close_before_first_use_does_not_construct_providers` | 4 | Deviation: "Closing the client" |
| `test_failed_construction_is_not_cached` | 4 | `crates/adapter/tests/lifecycle.rs::lifecycle_failed_build_is_not_cached_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_failed_build_is_not_cached_anthropic` |
| `test_environment_is_captured_on_first_use` | 4 | `crates/adapter/tests/lifecycle.rs::lifecycle_environment_is_read_on_first_use_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_environment_is_read_on_first_use_anthropic` |
| `test_async_cleanup_propagates_cancellation_after_remaining_cleanup` | 3 | Deviation: "Closing the client" |
| `test_concurrent_close_waits_for_same_cleanup` | 8 | Deviation: "Closing the client" |
| `test_cancelling_close_waiter_does_not_interrupt_cleanup` | 1 | Deviation: "Closing the client" |
| `test_concurrent_first_use_reuses_pool_and_isolates_traces` | 4 | `crates/adapter/tests/lifecycle.rs::lifecycle_concurrent_first_use_builds_one_openai`, `crates/adapter/tests/lifecycle.rs::lifecycle_concurrent_first_use_builds_one_anthropic` |

## `tests/test_provider_nonanswers.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_chat_completion_finish_reason` | 28 | `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_stop_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_stop_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_none_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_none_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_length_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_length_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_content_filter_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_content_filter_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_tool_calls_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_tool_calls_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_function_call_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_function_call_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_unknown_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_chat_finish_reason_unknown_native` |
| `test_openai_missing_usage` | 64 | `crates/adapter/tests/providers_openai.rs::usage_case_omitted_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_omitted_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_omitted_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_omitted_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_null_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_null_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_null_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_null_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_input_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_input_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_input_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_input_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_null_input_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_null_input_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_null_input_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_null_input_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_output_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_output_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_output_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_missing_output_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_null_output_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_null_output_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_null_output_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_null_output_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_zero_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_zero_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_zero_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_zero_responses_native`, `crates/adapter/tests/providers_openai.rs::usage_case_present_chat_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_present_chat_native`, `crates/adapter/tests/providers_openai.rs::usage_case_present_responses_prompted`, `crates/adapter/tests/providers_openai.rs::usage_case_present_responses_native` |
| `test_anthropic_nonanswers` | 36 | `crates/adapter/tests/providers_anthropic.rs::nonanswer_end_turn_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_end_turn_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_stop_sequence_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_stop_sequence_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_no_stop_reason_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_no_stop_reason_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_refusal_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_refusal_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_model_context_window_exceeded_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_model_context_window_exceeded_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_pause_turn_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_pause_turn_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_tool_use_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_tool_use_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_unknown_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_unknown_prompted`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_max_tokens_native`, `crates/adapter/tests/providers_anthropic.rs::nonanswer_max_tokens_prompted` |
| `test_openai_responses_refusal` | 8 | `crates/adapter/tests/providers_openai.rs::nonanswer_responses_refusal_alone_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_responses_refusal_alone_native`, `crates/adapter/tests/providers_openai.rs::nonanswer_responses_refusal_after_valid_text_prompted`, `crates/adapter/tests/providers_openai.rs::nonanswer_responses_refusal_after_valid_text_native` |

## `tests/test_provider_requests.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_openai_native_response_format_wraps_schema` | 1 | `crates/adapter/src/provider/openai_tests.rs::the_native_response_format_wraps_the_schema` |
| `test_openai_prompted_sends_no_response_format` | 1 | `crates/adapter/src/provider/openai_tests.rs::prompted_mode_sends_no_response_format` |
| `test_openai_result_reads_content_and_usage` | 1 | `crates/adapter/src/provider/openai_tests.rs::a_chat_reply_gives_its_content_and_its_usage` |
| `test_anthropic_request_puts_schema_in_output_config_when_structured` | 1 | `crates/adapter/src/provider/anthropic_tests.rs::a_structured_request_puts_the_schema_in_output_config` |
| `test_anthropic_request_omits_output_config_when_prompted` | 1 | `crates/adapter/src/provider/anthropic_tests.rs::a_prompted_request_has_no_output_config` |
| `test_gemini_request_puts_schema_in_response_format_when_structured` | 1 | `crates/adapter/src/provider/gemini_tests.rs::request_puts_schema_in_response_format_when_structured` |
| `test_gemini_request_omits_response_format_when_prompted` | 1 | `crates/adapter/src/provider/gemini_tests.rs::request_omits_response_format_when_prompted` |
| `test_gemini_request_sends_correction_turns_as_steps` | 1 | `crates/adapter/src/provider/gemini_tests.rs::request_sends_correction_turns_as_steps` |
| `test_gemini_result_reads_output_text_and_usage` | 1 | `crates/adapter/src/provider/gemini_tests.rs::result_reads_output_text_and_usage` |
| `test_gemini_incomplete_status_is_not_treated_as_an_answer` | 4 | `crates/adapter/src/provider/gemini_tests.rs::incomplete_status_is_not_an_answer` |
| `test_gemini_omitted_usage_is_not_treated_as_an_answer` | 1 | `crates/adapter/src/provider/gemini_tests.rs::omitted_usage_is_not_an_answer` |
| `test_anthropic_result_joins_text_blocks_and_reads_usage` | 1 | `crates/adapter/src/provider/anthropic_tests.rs::a_reply_joins_its_text_blocks_and_reads_the_usage` |
| `test_anthropic_output_limit` | 8 | `crates/adapter/src/provider/anthropic_tests.rs::a_reply_cut_at_the_output_limit_is_not_an_answer`, `crates/adapter/tests/providers_anthropic.rs::a_reply_cut_at_the_output_limit_is_not_asked_for_again` |
| `test_anthropic_rejects_nonpositive_output_limit` | 4 | `crates/adapter/src/provider/anthropic_tests.rs::a_zero_output_limit_is_refused` |
| `test_build_providers_select_gemini` | 1 | `crates/adapter/src/provider/factory_tests.rs::a_gemini_name_builds_a_gemini_provider` |
| `test_unknown_provider_is_rejected` | 1 | `crates/adapter/src/provider/factory_tests.rs::an_unknown_provider_name_is_rejected` |

## `tests/test_provider_retries.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_retry_policy_controls_http_attempts` | 12 | `crates/adapter/tests/providers_anthropic.rs::retry_budget_of_zero_sends_one_request`, `crates/adapter/tests/providers_anthropic.rs::retry_budget_of_one_sends_two_requests`, `crates/adapter/tests/providers_gemini.rs::retry_budget_of_zero_is_one_request`, `crates/adapter/tests/providers_gemini.rs::retry_budget_of_one_is_two_requests`, `crates/adapter/tests/providers_openai.rs::retry_budget_of_zero_sends_one_request`, `crates/adapter/tests/providers_openai.rs::retry_budget_of_one_sends_two_requests` |

## `tests/test_schema.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_invalid_dictionary_questions_are_rejected` | 4 | `crates/adapter/src/model_tests.rs::invalid_dictionary_questions_are_rejected` |
| `test_sdk_question_fields_are_revalidated` | 1 | `crates/adapter/src/model_tests.rs::sdk_question_fields_are_revalidated` |
| `test_question_ids_preserve_arbitrary_names` | 2 | `crates/adapter/src/decode_tests.rs::question_ids_preserve_arbitrary_names`, `crates/adapter/src/schema_tests.rs::question_ids_preserve_arbitrary_names` |
| `test_probability_labels_preserve_arbitrary_names` | 1 | `crates/adapter/src/decode_tests.rs::probability_labels_preserve_arbitrary_names`, `crates/adapter/src/schema_tests.rs::probability_labels_preserve_arbitrary_names` |
| `test_output_validation_preserves_types_bounds_and_allowed_values` | 13 | `crates/adapter/src/decode_tests.rs::output_validation_preserves_types_bounds_and_allowed_values` |
| `test_output_rejects_extra_fields_and_internal_field_names` | 3 | `crates/adapter/src/decode_tests.rs::output_rejects_extra_fields_and_internal_field_names` |

## `tests/utils/test_confidence_metrics.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_confidence_metrics` | 8 | `crates/adapter/src/metrics_tests.rs::metrics_upstream_confidence` |

## `tests/utils/test_error_handling.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_status_errors_map_and_preserve_status_and_body` | 18 | `crates/adapter/src/provider/http_tests.rs::http_status_400_is_a_bad_request`, `crates/adapter/src/provider/http_tests.rs::http_status_401_is_an_authentication_error`, `crates/adapter/src/provider/http_tests.rs::http_status_403_is_a_permission_error`, `crates/adapter/src/provider/http_tests.rs::http_status_429_is_a_rate_limit_error`, `crates/adapter/src/provider/http_tests.rs::http_status_500_is_a_server_error`, `crates/adapter/src/provider/http_tests.rs::http_status_418_is_another_api_error` |
| `test_timeout_and_connection_errors_map` | 3 | `crates/adapter/src/provider/http_tests.rs::http_deadline_passing_is_a_timeout`, `crates/adapter/src/provider/http_tests.rs::http_connect_refused_is_a_connection_error` |
| `test_unknown_and_sdk_errors_pass_through` | 3 | Deviation: "No translation of provider-SDK exceptions" |
| `test_translating_context_manager_reraises_translated_error` | 1 | Deviation: "No translation of provider-SDK exceptions" |
| `test_retries_succeed_after_transient_error` | 1 | `crates/adapter/src/retry_tests.rs::retries_succeed_after_a_transient_error` |
| `test_non_retryable_error_is_not_retried` | 1 | `crates/adapter/src/retry_tests.rs::a_non_retryable_error_is_not_retried` |
| `test_retries_are_exhausted_and_reasons_recorded` | 1 | `crates/adapter/src/retry_tests.rs::retries_are_exhausted_and_the_reasons_recorded` |

## `tests/utils/test_probability_normalization.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_probability_normalization_and_debug_data` | 3 | `crates/adapter/src/metrics_tests.rs::metrics_upstream_normalization` |
