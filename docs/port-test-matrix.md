# Upstream test matrix

Every test function of the Python SDK's `tests/test_*.py`
([typesafe-sdk-python](https://github.com/typesafe-ai/typesafe-sdk-python) 0.7.2, commit
`f078f1e`) and where its behaviour is covered here: a Rust test that exists in this repository,
named `path::function`, or a row of the deviations table in [`README.md`](../README.md), quoted
by its first cell; a test of the Python repository's own tooling is listed as excluded, with its
reason. `.github/scripts/port-test-matrix.py` checks every row; its docstring lists the checks.

A parametrized upstream test is one row, and **Cases** is the number of cases pytest collects
for it. Upstream's `clients` fixture runs most client tests twice, once with the synchronous client
and once with the asynchronous one: each such row maps to the Rust test of the asynchronous
client, and the synchronous half is covered, once for all of them, by the README deviation row
"Synchronous `TypeSafeClient`" (this crate has no blocking client).

Five upstream files test the Python repository's own tooling rather than the SDK's behaviour.
Each of their functions is listed, with the file's reason, in the last section,
[Excluded: tooling of the Python repository](#excluded-tooling-of-the-python-repository).

## Counts

| Upstream file | Functions | Rust test | Deviation row | Excluded |
| --- | ---: | ---: | ---: | ---: |
| `tests/test_clients.py` | 21 | 21 | 0 | 0 |
| `tests/test_config.py` | 11 | 8 | 3 | 0 |
| `tests/test_errors.py` | 6 | 4 | 2 | 0 |
| `tests/test_integration.py` | 3 | 3 | 0 | 0 |
| `tests/test_logging.py` | 8 | 5 | 3 | 0 |
| `tests/test_pydantic_response_models.py` | 5 | 5 | 0 | 0 |
| `tests/test_questions.py` | 11 | 7 | 4 | 0 |
| `tests/test_responses.py` | 15 | 11 | 4 | 0 |
| `tests/test_retry.py` | 25 | 25 | 0 | 0 |
| `tests/test_types.py` | 6 | 5 | 1 | 0 |
| `tests/test_release_notes.py` | 2 | 0 | 0 | 2 |
| `tests/test_docs.py` | 2 | 0 | 0 | 2 |
| `tests/test_typing.py` | 1 | 0 | 0 | 1 |
| `tests/test_public_api_surface.py` | 3 | 0 | 0 | 3 |
| `tests/test_public_sync.py` | 10 | 0 | 0 | 10 |

## `tests/test_clients.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_round_trip` | 6 | `tests/client.rs::round_trip_sends_the_body_and_decodes_every_answer_kind`, `src/question_tests.rs::typed_raw_and_mixed_forms_agree` |
| `test_extra_body_shallow_override` | 2 | `tests/client.rs::extra_body_shallow_override`, `src/request_tests.rs::extra_members_merge_last_write_wins_and_replace_a_built_in_in_place` |
| `test_unserializable_request_body_raises` | 2 | `tests/client.rs::unserializable_request_body_raises_before_the_network`, `src/request_tests.rs::a_value_that_cannot_be_encoded_fails_before_anything_is_sent` |
| `test_raw_question_passthrough` | 2 | `tests/client.rs::raw_question_passthrough`, `src/question_tests.rs::raw_questions_pass_through_beside_typed_ones` |
| `test_question_schema_validation_is_left_to_api` | 4 | `tests/client.rs::question_schema_validation_is_left_to_api`, `src/question_tests.rs::raw_question_schema_is_left_to_the_api` |
| `test_rich_descriptions` | 2 | `tests/client.rs::rich_descriptions`, `src/question_tests.rs::object_content_is_spliced_in_unchanged` |
| `test_models_shape` | 2 | `tests/models.rs::models_shape`, `src/models_tests.rs::a_models_response_decodes_to_its_cards` |
| `test_models_ignore_unknown_fields` | 2 | `tests/models.rs::models_ignore_unknown_fields`, `src/models_tests.rs::members_a_card_does_not_model_are_dropped_but_stay_in_the_raw_body` |
| `test_invalid_models_response` | 8 | `tests/models.rs::invalid_models_response`, `src/models_tests.rs::a_models_body_of_the_wrong_shape_is_a_validation_error_at_the_offending_field` |
| `test_validation_before_network` | 4 | `tests/client.rs::validation_before_network`, `src/question_tests.rs::an_empty_set_is_rejected` |
| `test_error_mapping` | 22 | `tests/client.rs::error_mapping` |
| `test_error_messages` | 16 | `tests/client.rs::error_messages`, `tests/client.rs::server_text_in_an_api_error_is_escaped_and_cut` |
| `test_transport_errors` | 12 | `tests/client.rs::transport_errors_are_connection_errors_with_their_cause`, `tests/client.rs::an_attempt_past_its_deadline_is_a_timeout_with_that_deadline` |
| `test_system_one_timeout_override` | 8 | `tests/client.rs::system_one_timeout_override` |
| `test_headers_timeout_and_logging` | 2 | `tests/client.rs::headers_timeout_and_logging`, `tests/client.rs::the_upstream_base_url_with_a_prefix_and_trailing_slashes` |
| `test_http_client_settings` | 2 | `tests/client.rs::a_custom_transport_carries_every_request_with_the_sdk_headers` |
| `test_supplied_network_resources_closed` | 8 | `tests/client.rs::the_last_clone_of_a_client_drops_its_transport` |
| `test_owned_http_client_closed` | 1 | `tests/client.rs::the_last_clone_of_a_client_drops_its_transport` |
| `test_exceptional_context_closes_http_client` | 4 | `tests/client.rs::the_last_clone_of_a_client_drops_its_transport` |
| `test_task_cancellation_closes_context` | 1 | `tests/client.rs::dropping_a_call_in_flight_cancels_it` |
| `test_cancellation_propagates` | 1 | `tests/client.rs::dropping_a_call_in_flight_cancels_it` |

## `tests/test_config.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_transport_and_http_client_mutually_exclusive` | 2 | Deviation: "`http_client=` or `transport=`, mutually exclusive" |
| `test_model_override` | 4 | `src/request_tests.rs::the_body_is_state_model_and_questions_in_that_order`, `src/config_tests.rs::each_setting_comes_from_the_caller_then_the_environment_then_the_default` |
| `test_resolution` | 6 | `src/config_tests.rs::each_setting_comes_from_the_caller_then_the_environment_then_the_default`, `src/client_tests.rs::settings_the_builder_leaves_unset_come_from_the_environment` |
| `test_missing_key` | 6 | `src/config_tests.rs::a_missing_or_blank_environment_key_is_the_upstream_error` |
| `test_api_key_whitespace` | 16 | `src/config_tests.rs::a_padded_key_is_trimmed_as_python_strips_it_from_either_source` |
| `test_invalid_explicit_key_does_not_fall_back_to_env` | 8 | `src/config_tests.rs::an_invalid_explicit_key_does_not_fall_back_to_the_environment` |
| `test_invalid_api_key` | 32 | `src/config_tests.rs::a_key_outside_printable_ascii_is_refused_without_repeating_it` |
| `test_empty_env_unset` | 2 | `src/config_tests.rs::blank_environment_values_count_as_unset_and_the_log_level_is_never_read` |
| `test_invalid_timeout` | 8 | `src/config_tests.rs::a_zero_timeout_is_the_upstream_error_and_any_positive_one_is_kept`, `src/request_tests.rs::a_deadline_is_the_clients_a_calls_or_none_and_never_zero` |
| `test_timeout_object` | 2 | Deviation: "Timeout per httpx phase; `httpx.Timeout` objects" |
| `test_http_client_timeout_precedence` | 16 | Deviation: "`http_client.timeout` takes precedence" |

## `tests/test_errors.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_exception_reconstruction` | 14 | Deviation: "Responses and errors are picklable and copyable" |
| `test_api_error_from_process_pool` | 1 | Deviation: "Responses and errors are picklable and copyable" |
| `test_api_error_request_context` | 4 | `tests/client.rs::error_mapping`, `src/error_tests.rs::a_failure_renders_as_the_endpoint_the_status_the_message_and_the_request_id` |
| `test_api_error_endpoint_omits_url_credentials` | 1 | `src/error_tests.rs::an_endpoint_drops_the_credentials_the_query_and_the_default_port` |
| `test_message_override` | 16 | `src/error_tests.rs::a_caller_supplied_message_replaces_the_one_in_the_body` |
| `test_error_body_edge_cases` | 18 | `src/error_tests.rs::a_body_that_is_not_json_is_reported_as_the_text_it_is`, `src/error_tests.rs::a_json_string_body_is_its_own_message_and_an_empty_one_leaves_the_status_alone`, `src/error_tests.rs::a_long_body_used_as_its_own_message_is_cut_and_marked` |

## `tests/test_integration.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_live_models` | 2 | `crates/live-tests::live_models` |
| `test_live_questions` | 2 | `crates/live-tests::live_questions` |
| `test_live_pydantic_response` | 2 | `crates/live-tests::live_typed_response` |

## `tests/test_logging.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_secret_headers_redacted` | 54 | `tests/client.rs::every_secret_header_spelling_is_redacted_both_ways`, `src/telemetry_tests.rs::every_upstream_secret_header_is_redacted_and_its_neighbour_is_not` |
| `test_transport_errors_do_not_expose_credentials` | 40 | `tests/client.rs::transport_errors_never_expose_a_credential`, `tests/client.rs::transport_errors_never_log_a_credential` |
| `test_exception_redaction_escaped_values` | 4 | `src/redact_tests.rs::every_escaped_form_of_a_credential_is_replaced` |
| `test_exception_redaction_shared_causes_cycles_and_notes` | 1 | Deviation: "Redacted exception copies keep their type, `__notes__`, `__context__` and shared or cyclic causes" |
| `test_exception_redaction_structured_constructor` | 1 | Deviation: "Redacted exception copies keep their type, `__notes__`, `__context__` and shared or cyclic causes" |
| `test_exception_redaction_preserves_network_diagnostics` | 1 | `src/redact_tests.rs::a_chain_without_a_credential_is_kept_as_it_is` |
| `test_logger_level_controls_output` | 6 | `tests/client.rs::every_attempt_gets_one_info_line_and_no_body_reaches_info_or_debug` |
| `test_setup_logging_from_env` | 5 | Deviation: "`TYPESAFE_LOG_LEVEL` sets the logger level" |

## `tests/test_pydantic_response_models.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_standalone_pydantic_response_model` | 4 | `tests/derive.rs::ask_sends_the_compiled_questions_and_decodes_into_the_struct` |
| `test_explicit_default_response_model` | 4 | `tests/client.rs::round_trip_sends_the_body_and_decodes_every_answer_kind` |
| `test_pydantic_system_one_response_subclass` | 2 | `tests/derive.rs::extra_answers_are_skipped_and_the_first_answer_of_a_name_wins` |
| `test_pydantic_response_validation` | 4 | `tests/derive.rs::a_missing_or_wrong_answer_fails_at_its_field` |
| `test_custom_response_preserves_api_errors` | 2 | `tests/derive.rs::an_asked_request_is_retried_by_the_policy_that_applies_to_it` |

## `tests/test_questions.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_normalization_preserves_objects` | 1 | `src/question_tests.rs::typed_questions_encode_to_the_upstream_wire_form` |
| `test_normalization_preserves_raw_questions` | 3 | `src/question_tests.rs::raw_questions_pass_through_beside_typed_ones` |
| `test_raw_questions_require_structural_keys` | 10 | `src/question_tests.rs::a_raw_question_needs_a_nonempty_string_type_and_criteria_where_upstream_does` |
| `test_direct_encoding_omits_only_default_fields` | 5 | `src/question_tests.rs::unset_members_are_left_off_the_wire` |
| `test_discriminators_are_automatic` | 1 | `src/question_tests.rs::each_builder_writes_its_own_type_tag` |
| `test_invalid_typed_question_is_rejected_on_construction` | 1 | Deviation: "Unknown fields rejected on typed questions; `RetryPolicy` field types checked at run time" |
| `test_typed_questions_reject_unknown_fields` | 3 | Deviation: "Unknown fields rejected on typed questions; `RetryPolicy` field types checked at run time" |
| `test_optional_noul_criteria` | 12 | `src/question_tests.rs::noul_criteria_carry_either_outcome_or_both`, `src/question_tests.rs::raw_noul_criteria_pass_through_as_given` |
| `test_typed_noul_criteria_reject_unknown_fields` | 1 | Deviation: "Unknown fields rejected on typed questions; `RetryPolicy` field types checked at run time" |
| `test_empty_score_criteria_is_rejected` | 2 | `src/question_tests.rs::a_score_without_levels_is_rejected_typed_or_raw` |
| `test_covariant_question_mappings` | 2 | Deviation: "Covariant `Mapping` question inputs" |

## `tests/test_responses.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_malformed_response_raises_validation_error` | 16 | `src/de_tests.rs::a_malformed_response_fails_where_the_python_sdk_says_it_does` |
| `test_nested_missing_field_path` | 3 | `src/models_tests.rs::a_missing_member_of_a_later_card_is_named_with_its_index` |
| `test_response_carries_request_id` | 2 | `tests/client.rs::round_trip_sends_the_body_and_decodes_every_answer_kind`, `src/response_tests.rs::the_meta_keeps_the_http_response_and_prints_none_of_its_contents` |
| `test_response_carries_raw_http_response` | 2 | `tests/client.rs::round_trip_sends_the_body_and_decodes_every_answer_kind` |
| `test_response_serialization_excludes_http_metadata` | 4 | `src/response_tests.rs::serializing_a_response_writes_the_payload_and_not_the_http_metadata`, `src/models_tests.rs::serializing_a_models_response_writes_the_payload_and_not_the_http_metadata` |
| `test_copied_response_preserves_metadata` | 1 | Deviation: "Responses and errors are picklable and copyable" |
| `test_missing_raw_raises_on_access` | 1 | Deviation: "A `SystemOneResponse` can be built without its HTTP response" |
| `test_missing_request_id_raises_on_access` | 2 | `src/response_tests.rs::a_request_id_that_is_absent_or_not_text_is_none` |
| `test_unknown_extra_fields_tolerated` | 2 | `src/de_tests.rs::members_this_version_does_not_know_are_ignored` |
| `test_unknown_answer_type_ignored` | 2 | `src/de_tests.rs::a_future_answer_type_is_skipped_rather_than_raised`, `src/de_tests.rs::an_answer_of_an_unknown_type_is_dropped_and_stays_in_the_raw_body` |
| `test_response_preserves_nested_json` | 1 | `src/response_tests.rs::a_structured_legend_description_serializes_as_data` |
| `test_answer_attributes_and_dictionary_types` | 1 | `src/response_tests.rs::each_answer_serializes_with_its_type` |
| `test_public_response_types_ignore_unknown_fields` | 7 | `src/de_tests.rs::members_this_version_does_not_know_are_ignored`, `src/models_tests.rs::members_a_card_does_not_model_are_dropped_but_stay_in_the_raw_body` |
| `test_answer_fields_are_frozen` | 3 | Deviation: "Frozen pydantic models" |
| `test_answer_groups_are_cached_and_not_serialized` | 3 | Deviation: "`.nouls` / `.choices` / `.scores` as cached dict copies" |

## `tests/test_retry.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_retry_policy_invalid_timeout` | 4 | `src/retry_tests.rs::a_zero_budget_is_refused_as_upstream_test_retry_policy_invalid_timeout` |
| `test_zero_backoff_retries` | 12 | `src/retry_tests.rs::zero_backoff_retries_at_once_as_upstream_test_zero_backoff_retries` |
| `test_invalid_backoff` | 6 | `src/retry_tests.rs::the_largest_backoff_degrades_as_upstream_test_invalid_backoff` |
| `test_invalid_backoff_jitter` | 4 | `src/retry_tests.rs::a_jitter_outside_zero_to_one_is_refused_as_upstream_test_invalid_backoff_jitter` |
| `test_invalid_max_retries` | 4 | `src/retry_tests.rs::the_largest_retry_count_is_accepted_as_upstream_test_invalid_max_retries` |
| `test_retry_policy_timeout_budget` | 24 | `src/retry_tests.rs::the_budget_stops_retrying_as_upstream_test_retry_policy_timeout_budget` |
| `test_retry_policy_timeout_override` | 4 | `src/retry_tests.rs::a_calls_budget_replaces_the_clients_as_upstream_test_retry_policy_timeout_override` |
| `test_default_retry_statuses` | 24 | `src/retry_tests.rs::the_default_statuses_are_retried_as_upstream_test_default_retry_statuses` |
| `test_connection_retry_recovers` | 8 | `src/retry_tests.rs::a_transport_failure_is_retried_as_upstream_test_connection_retry_recovers`, `src/retry_tests.rs::a_timeout_is_retried_as_upstream_test_connection_retry_recovers`, `src/retry_tests.rs::a_failure_before_sending_is_retried_as_upstream_test_connection_retry_recovers` |
| `test_server_delay_through_tenacity` | 8 | `src/retry_tests.rs::the_servers_delay_is_waited_as_upstream_test_server_delay_through_tenacity` |
| `test_parse_retry_after` | 9 | `src/error_tests.rs::retry_after_reproduces_every_case_the_python_sdk_pins`, `src/error_tests.rs::retry_after_reads_an_http_date_against_the_instant_it_is_given` |
| `test_backoff_dates_cap_and_jitter` | 1 | `src/retry_tests.rs::delays_follow_upstream_test_backoff_dates_cap_and_jitter`, `src/retry_tests.rs::default_schedule_doubles_then_caps_as_upstream_test_backoff_dates_cap_and_jitter` |
| `test_system_one_retry_override` | 4 | `src/retry_tests.rs::a_calls_policy_replaces_the_clients_as_upstream_test_system_one_retry_override` |
| `test_async_concurrent_retry_state` | 1 | `src/retry_tests.rs::concurrent_calls_count_their_own_retries_as_upstream_test_async_concurrent_retry_state` |
| `test_system_one_retry_recovers_with_overrides` | 8 | `src/retry_tests.rs::a_call_recovers_with_its_overrides_as_upstream_test_system_one_retry_recovers_with_overrides` |
| `test_concurrent_system_one_overrides` | 1 | `src/retry_tests.rs::concurrent_calls_keep_their_own_policies_as_upstream_test_concurrent_system_one_overrides` |
| `test_exhausted_transport_retry` | 4 | `src/retry_tests.rs::the_last_connection_failure_is_returned_as_upstream_test_exhausted_transport_retry`, `src/retry_tests.rs::the_last_timeout_is_returned_as_upstream_test_exhausted_transport_retry` |
| `test_exhausted_retry_preserves_final_http_error` | 2 | `src/retry_tests.rs::the_last_api_error_is_returned_whole_as_upstream_test_exhausted_retry_preserves_final_http_error` |
| `test_cancel_pending_retry` | 1 | `src/retry_tests.rs::dropping_a_waiting_call_cancels_its_retry_as_upstream_test_cancel_pending_retry` |
| `test_retry_policy_max_retries` | 6 | `src/retry_tests.rs::max_retries_counts_attempts_as_upstream_test_retry_policy_max_retries` |
| `test_retry_policy_custom_statuses` | 4 | `src/retry_tests.rs::custom_statuses_replace_the_default_as_upstream_test_retry_policy_custom_statuses` |
| `test_retry_policy_per_call_override` | 2 | `src/retry_tests.rs::a_calls_count_replaces_the_clients_as_upstream_test_retry_policy_per_call_override` |
| `test_retry_policy_exceptions_and_predicate` | 4 | `src/retry_tests.rs::a_predicate_opts_a_failure_in_as_upstream_test_retry_policy_exceptions_and_predicate` |
| `test_retry_policy_wait_options` | 1 | `src/retry_tests.rs::the_delay_follows_the_wait_options_as_upstream_test_retry_policy_wait_options` |
| `test_backoff_extreme_values` | 4 | `src/retry_tests.rs::extreme_values_match_upstream_test_backoff_extreme_values`, `src/retry_tests.rs::extreme_durations_come_through_the_policy_as_upstream_test_backoff_extreme_values` |

## `tests/test_types.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_str_subclasses_fallback_to_strings` | 1 | Deviation: "`str` subclasses and abstract `Mapping` / `Sequence` inputs" |
| `test_json_value_and_state_exclude_top_level_none` | 1 | `src/request_tests.rs::a_state_that_is_not_text_an_object_or_an_array_is_refused` |
| `test_array_inputs` | 4 | `src/question_tests.rs::array_content_is_accepted_everywhere_content_is` |
| `test_raw_optional_fields_preserve_explicit_null` | 2 | `src/question_tests.rs::a_raw_question_keeps_an_explicit_null` |
| `test_explicitly_nullable_json_values` | 2 | `src/question_tests.rs::a_null_inside_content_survives` |
| `test_abstract_input_containers_encode` | 2 | `src/question_tests.rs::builders_take_any_iterator` |

## Excluded: tooling of the Python repository

| Upstream file | Upstream test | Reason |
| --- | --- | --- |
| `tests/test_release_notes.py` | `test_release_notes` | the release-notes script that cuts a section of `docs/changelog.md` |
| `tests/test_release_notes.py` | `test_invalid_release_notes` | the release-notes script that cuts a section of `docs/changelog.md` |
| `tests/test_docs.py` | `test_markdown` | runs the Markdown and docstring examples of the Python documentation against the API |
| `tests/test_docs.py` | `test_python_doctests` | runs the Markdown and docstring examples of the Python documentation against the API |
| `tests/test_typing.py` | `test_public_typing` | Python static type checking (pyrefly) of typing fixtures |
| `tests/test_public_api_surface.py` | `test_public_members` | snapshots of the Python module's export list and constructor keywords |
| `tests/test_public_api_surface.py` | `test_package_exports` | snapshots of the Python module's export list and constructor keywords |
| `tests/test_public_api_surface.py` | `test_constructor_kwargs` | snapshots of the Python module's export list and constructor keywords |
| `tests/test_public_sync.py` | `test_sign_snapshot_and_push` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_signing_failure_keeps_refs` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_dry_run_skips_github` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_snapshot_and_push_retries` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_existing_history_deletions_and_immutable_tags` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_invalid_includes` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_unsafe_snapshots` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_version_mismatch` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_atomic_push_rejects_concurrent_update` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
| `tests/test_public_sync.py` | `test_release_contributors` | the scripts that mirror the private repository to the public one and sign releases (`sync_public.py`, `push_public.py`); upstream skips it outside its private repository |
