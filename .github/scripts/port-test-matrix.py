#!/usr/bin/env python3
"""Check that each port's test matrix maps every upstream test to something real.

A matrix names, for each test function of an upstream Python test suite, the
Rust tests that cover its behaviour or the row of a README's deviations table
that explains why there is none. A mapping that names a test which was renamed
or deleted, or a deviation row that was reworded, still reads as a mapping, so
nothing but a check notices that it has stopped being one. This is that check.

Each upstream is one ``Upstream`` in ``UPSTREAMS``: its matrix, its README and
deviations table, its pin, the files whose functions may be excluded and the
columns of its Counts table. Every check below runs for each of them.

It fails on:

* a row whose target cell is empty, or holds neither Rust tests nor a
  deviation, an excluded row without a reason, an excluded row of a file that
  is not one of the upstream's excludable files (with none, no row may be
  excluded), and an excluded row with a Cases cell;
* a Rust target ``path::name`` when ``path`` (a file, or a directory searched
  recursively) does not exist or has no ``fn name`` carrying a test attribute;
* a quoted deviation that is not the first cell of a row of the deviations
  table in the upstream's README;
* an upstream test (the pin: ``(file, name) -> cases`` of every upstream
  ``test_*`` function, so that no upstream checkout is needed) without a row,
  a row naming a pair that is not in it, and an upstream test named twice;
* a row that is not excluded whose Cases is not a positive number, or differs
  from the pin's (``None`` in the pin: an excluded test, which has no Cases);
* per-file counts that differ from the ones the matrix's Counts table states,
  in the columns the upstream's Counts layout names. The rows are held to the
  pin by name and by Cases, so a Counts line whose functions or cases differ
  from the pin's sums fails through the rows;
* for an upstream with annotated directories (the Python adapter's two
  crates), an ``// Upstream:`` line in a Rust source under them that is not
  ``// Upstream: <file>::<function>``, that is not in the block of
  attributes and comments directly above a test function, that names no
  row's function, or that sits above a test its row does not list; and a
  Rust target of a row, under those directories, whose test carries no line
  naming that row's function. A target outside them (the Python adapter's
  rows name three tests of the SDK) needs no line. Each of these faults names
  the file and the line.

A row is one of three kinds: mapped to Rust tests, mapped to a deviation row,
or excluded with a reason (for the Python SDK, only the functions of the
upstream files that test the Python repository's own tooling; for the Python
adapter, none).

With ``--upstream <name>=<checkout>`` it also checks the pin of the upstream
called ``name`` itself: every ``test_*`` function the checkout's
``tests/test_*.py`` and ``tests/utils/test_*.py`` define is pinned, and every
pinned one is defined. It also fails on:

* an upstream test file the pin does not name that defines no module-level
  ``test_*`` function (none at all, or only methods of a class), which the pin
  cannot cover.

Run it from the repository root.
"""

import argparse
import re
import sys
from collections import Counter
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from types import MappingProxyType

#: A section of a matrix: ``## `tests/test_x.py` `` or
#: ``## `tests/utils/test_x.py` ``.
SECTION = re.compile(r"^## `(tests/(?:utils/)?test_\w+\.py)`\s*$")
#: A Rust target: a backticked ``path::function``.
RUST_TARGET = re.compile(r"`([\w./-]+)::(\w+)`")
#: A deviation target: ``Deviation: "<first cell of a README row>"``.
DEVIATION_TARGET = re.compile(r'^Deviation: "(.+)"$')
#: How an excluded row's target starts; the file's reason follows.
EXCLUDED_PREFIX = "Excluded: "
#: A test function defined at module level of an upstream file.
UPSTREAM_TEST = re.compile(r"^(?:async )?def (test_\w+)\(", re.MULTILINE)
#: An attribute that makes the function after it a test.
TEST_ATTRIBUTE = re.compile(r"#\[(?:[\w:]+::)?test\b")
#: A comment that says which upstream function the Rust test below it ports.
UPSTREAM_COMMENT = re.compile(r"^\s*// Upstream:")
#: The one form that comment may take; the upstream file and function.
UPSTREAM_LINE = re.compile(
    r"^\s*// Upstream: (tests/(?:utils/)?test_\w+\.py)::(test_\w+)\s*$"
)
#: The first line of a function; the name is group 1.
FUNCTION = re.compile(r"^\s*(?:pub(?:\([\w:]+\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*[(<]")
#: The upstream test files ``--upstream`` reads, relative to the checkout.
UPSTREAM_GLOBS = ("tests/test_*.py", "tests/utils/test_*.py")
#: How each column a Counts table may have reads in a fault message.
COUNTS_LABELS = {
    "functions": "functions",
    "cases": "cases",
    "rust": "Rust",
    "deviation": "deviation",
    "excluded": "excluded",
}

#: The Python SDK's upstream files that test the Python repository's own
#: tooling, the only ones whose functions may be excluded.
EXCLUDED_FILES = frozenset(
    {
        "tests/test_docs.py",
        "tests/test_public_api_surface.py",
        "tests/test_public_sync.py",
        "tests/test_release_notes.py",
        "tests/test_typing.py",
    }
)
#: Every ``test_*`` function the Python SDK's upstream files define at the
#: ported release (typesafe-sdk-python f078f1e, v0.7.2), as
#: ``(file, name) -> cases``: the number of cases pytest collects for it, or
#: ``None`` for an excluded function, whose row has no Cases cell. Pinned so
#: that a dropped, renamed or made-up row, or a changed Cases number, fails
#: without an upstream checkout; ``--upstream`` checks the names against a
#: checkout.
UPSTREAM_TESTS: Mapping[tuple[str, str], int | None] = MappingProxyType(
    {
        ("tests/test_clients.py", "test_cancellation_propagates"): 1,
        ("tests/test_clients.py", "test_error_mapping"): 22,
        ("tests/test_clients.py", "test_error_messages"): 16,
        ("tests/test_clients.py", "test_exceptional_context_closes_http_client"): 4,
        ("tests/test_clients.py", "test_extra_body_shallow_override"): 2,
        ("tests/test_clients.py", "test_headers_timeout_and_logging"): 2,
        ("tests/test_clients.py", "test_http_client_settings"): 2,
        ("tests/test_clients.py", "test_invalid_models_response"): 8,
        ("tests/test_clients.py", "test_models_ignore_unknown_fields"): 2,
        ("tests/test_clients.py", "test_models_shape"): 2,
        ("tests/test_clients.py", "test_owned_http_client_closed"): 1,
        ("tests/test_clients.py", "test_question_schema_validation_is_left_to_api"): 4,
        ("tests/test_clients.py", "test_raw_question_passthrough"): 2,
        ("tests/test_clients.py", "test_rich_descriptions"): 2,
        ("tests/test_clients.py", "test_round_trip"): 6,
        ("tests/test_clients.py", "test_supplied_network_resources_closed"): 8,
        ("tests/test_clients.py", "test_system_one_timeout_override"): 8,
        ("tests/test_clients.py", "test_task_cancellation_closes_context"): 1,
        ("tests/test_clients.py", "test_transport_errors"): 12,
        ("tests/test_clients.py", "test_unserializable_request_body_raises"): 2,
        ("tests/test_clients.py", "test_validation_before_network"): 4,
        ("tests/test_config.py", "test_api_key_whitespace"): 16,
        ("tests/test_config.py", "test_empty_env_unset"): 2,
        ("tests/test_config.py", "test_http_client_timeout_precedence"): 16,
        ("tests/test_config.py", "test_invalid_api_key"): 32,
        (
            "tests/test_config.py",
            "test_invalid_explicit_key_does_not_fall_back_to_env",
        ): 8,
        ("tests/test_config.py", "test_invalid_timeout"): 8,
        ("tests/test_config.py", "test_missing_key"): 6,
        ("tests/test_config.py", "test_model_override"): 4,
        ("tests/test_config.py", "test_resolution"): 6,
        ("tests/test_config.py", "test_timeout_object"): 2,
        (
            "tests/test_config.py",
            "test_transport_and_http_client_mutually_exclusive",
        ): 2,
        ("tests/test_docs.py", "test_markdown"): None,
        ("tests/test_docs.py", "test_python_doctests"): None,
        ("tests/test_errors.py", "test_api_error_endpoint_omits_url_credentials"): 1,
        ("tests/test_errors.py", "test_api_error_from_process_pool"): 1,
        ("tests/test_errors.py", "test_api_error_request_context"): 4,
        ("tests/test_errors.py", "test_error_body_edge_cases"): 18,
        ("tests/test_errors.py", "test_exception_reconstruction"): 14,
        ("tests/test_errors.py", "test_message_override"): 16,
        ("tests/test_integration.py", "test_live_models"): 2,
        ("tests/test_integration.py", "test_live_pydantic_response"): 2,
        ("tests/test_integration.py", "test_live_questions"): 2,
        ("tests/test_logging.py", "test_exception_redaction_escaped_values"): 4,
        (
            "tests/test_logging.py",
            "test_exception_redaction_preserves_network_diagnostics",
        ): 1,
        (
            "tests/test_logging.py",
            "test_exception_redaction_shared_causes_cycles_and_notes",
        ): 1,
        ("tests/test_logging.py", "test_exception_redaction_structured_constructor"): 1,
        ("tests/test_logging.py", "test_logger_level_controls_output"): 6,
        ("tests/test_logging.py", "test_secret_headers_redacted"): 54,
        ("tests/test_logging.py", "test_setup_logging_from_env"): 5,
        (
            "tests/test_logging.py",
            "test_transport_errors_do_not_expose_credentials",
        ): 40,
        ("tests/test_public_api_surface.py", "test_constructor_kwargs"): None,
        ("tests/test_public_api_surface.py", "test_package_exports"): None,
        ("tests/test_public_api_surface.py", "test_public_members"): None,
        (
            "tests/test_public_sync.py",
            "test_atomic_push_rejects_concurrent_update",
        ): None,
        ("tests/test_public_sync.py", "test_dry_run_skips_github"): None,
        (
            "tests/test_public_sync.py",
            "test_existing_history_deletions_and_immutable_tags",
        ): None,
        ("tests/test_public_sync.py", "test_invalid_includes"): None,
        ("tests/test_public_sync.py", "test_release_contributors"): None,
        ("tests/test_public_sync.py", "test_sign_snapshot_and_push"): None,
        ("tests/test_public_sync.py", "test_signing_failure_keeps_refs"): None,
        ("tests/test_public_sync.py", "test_snapshot_and_push_retries"): None,
        ("tests/test_public_sync.py", "test_unsafe_snapshots"): None,
        ("tests/test_public_sync.py", "test_version_mismatch"): None,
        (
            "tests/test_pydantic_response_models.py",
            "test_custom_response_preserves_api_errors",
        ): 2,
        (
            "tests/test_pydantic_response_models.py",
            "test_explicit_default_response_model",
        ): 4,
        (
            "tests/test_pydantic_response_models.py",
            "test_pydantic_response_validation",
        ): 4,
        (
            "tests/test_pydantic_response_models.py",
            "test_pydantic_system_one_response_subclass",
        ): 2,
        (
            "tests/test_pydantic_response_models.py",
            "test_standalone_pydantic_response_model",
        ): 4,
        ("tests/test_questions.py", "test_covariant_question_mappings"): 2,
        (
            "tests/test_questions.py",
            "test_direct_encoding_omits_only_default_fields",
        ): 5,
        ("tests/test_questions.py", "test_discriminators_are_automatic"): 1,
        ("tests/test_questions.py", "test_empty_score_criteria_is_rejected"): 2,
        (
            "tests/test_questions.py",
            "test_invalid_typed_question_is_rejected_on_construction",
        ): 1,
        ("tests/test_questions.py", "test_normalization_preserves_objects"): 1,
        ("tests/test_questions.py", "test_normalization_preserves_raw_questions"): 3,
        ("tests/test_questions.py", "test_optional_noul_criteria"): 12,
        ("tests/test_questions.py", "test_raw_questions_require_structural_keys"): 10,
        (
            "tests/test_questions.py",
            "test_typed_noul_criteria_reject_unknown_fields",
        ): 1,
        ("tests/test_questions.py", "test_typed_questions_reject_unknown_fields"): 3,
        ("tests/test_release_notes.py", "test_invalid_release_notes"): None,
        ("tests/test_release_notes.py", "test_release_notes"): None,
        ("tests/test_responses.py", "test_answer_attributes_and_dictionary_types"): 1,
        ("tests/test_responses.py", "test_answer_fields_are_frozen"): 3,
        (
            "tests/test_responses.py",
            "test_answer_groups_are_cached_and_not_serialized",
        ): 3,
        ("tests/test_responses.py", "test_copied_response_preserves_metadata"): 1,
        (
            "tests/test_responses.py",
            "test_malformed_response_raises_validation_error",
        ): 16,
        ("tests/test_responses.py", "test_missing_raw_raises_on_access"): 1,
        ("tests/test_responses.py", "test_missing_request_id_raises_on_access"): 2,
        ("tests/test_responses.py", "test_nested_missing_field_path"): 3,
        (
            "tests/test_responses.py",
            "test_public_response_types_ignore_unknown_fields",
        ): 7,
        ("tests/test_responses.py", "test_response_carries_raw_http_response"): 2,
        ("tests/test_responses.py", "test_response_carries_request_id"): 2,
        ("tests/test_responses.py", "test_response_preserves_nested_json"): 1,
        (
            "tests/test_responses.py",
            "test_response_serialization_excludes_http_metadata",
        ): 4,
        ("tests/test_responses.py", "test_unknown_answer_type_ignored"): 2,
        ("tests/test_responses.py", "test_unknown_extra_fields_tolerated"): 2,
        ("tests/test_retry.py", "test_async_concurrent_retry_state"): 1,
        ("tests/test_retry.py", "test_backoff_dates_cap_and_jitter"): 1,
        ("tests/test_retry.py", "test_backoff_extreme_values"): 4,
        ("tests/test_retry.py", "test_cancel_pending_retry"): 1,
        ("tests/test_retry.py", "test_concurrent_system_one_overrides"): 1,
        ("tests/test_retry.py", "test_connection_retry_recovers"): 8,
        ("tests/test_retry.py", "test_default_retry_statuses"): 24,
        ("tests/test_retry.py", "test_exhausted_retry_preserves_final_http_error"): 2,
        ("tests/test_retry.py", "test_exhausted_transport_retry"): 4,
        ("tests/test_retry.py", "test_invalid_backoff"): 6,
        ("tests/test_retry.py", "test_invalid_backoff_jitter"): 4,
        ("tests/test_retry.py", "test_invalid_max_retries"): 4,
        ("tests/test_retry.py", "test_parse_retry_after"): 9,
        ("tests/test_retry.py", "test_retry_policy_custom_statuses"): 4,
        ("tests/test_retry.py", "test_retry_policy_exceptions_and_predicate"): 4,
        ("tests/test_retry.py", "test_retry_policy_invalid_timeout"): 4,
        ("tests/test_retry.py", "test_retry_policy_max_retries"): 6,
        ("tests/test_retry.py", "test_retry_policy_per_call_override"): 2,
        ("tests/test_retry.py", "test_retry_policy_timeout_budget"): 24,
        ("tests/test_retry.py", "test_retry_policy_timeout_override"): 4,
        ("tests/test_retry.py", "test_retry_policy_wait_options"): 1,
        ("tests/test_retry.py", "test_server_delay_through_tenacity"): 8,
        ("tests/test_retry.py", "test_system_one_retry_override"): 4,
        ("tests/test_retry.py", "test_system_one_retry_recovers_with_overrides"): 8,
        ("tests/test_retry.py", "test_zero_backoff_retries"): 12,
        ("tests/test_types.py", "test_abstract_input_containers_encode"): 2,
        ("tests/test_types.py", "test_array_inputs"): 4,
        ("tests/test_types.py", "test_explicitly_nullable_json_values"): 2,
        ("tests/test_types.py", "test_json_value_and_state_exclude_top_level_none"): 1,
        ("tests/test_types.py", "test_raw_optional_fields_preserve_explicit_null"): 2,
        ("tests/test_types.py", "test_str_subclasses_fallback_to_strings"): 1,
        ("tests/test_typing.py", "test_public_typing"): None,
    }
)


@dataclass(frozen=True)
class Upstream:
    """One upstream test suite and the matrix that maps it.

    Attributes:
        name: The upstream repository; ``--upstream <name>=<checkout>`` names it.
        matrix: The matrix file, relative to the repository root.
        readme: The README that holds the deviations table.
        deviations_heading: The heading line of the deviations table's section.
        deviations_header: The first cell of the deviations table's header row,
            which is not a deviation.
        pin: ``(file, name) -> cases`` of every upstream ``test_*`` function;
            ``None`` for an excluded function, whose row has no Cases cell.
        pin_name: What fault messages call the pin.
        excludable: The upstream files whose functions may be excluded.
        counts_columns: The number columns of the Counts table after the file,
            in order, as ``Counts`` field names.
        annotated: The directories whose Rust tests carry an
            ``// Upstream: <file>::<function>`` line for the row that names
            them; empty when the upstream's tests carry none.
    """

    name: str
    matrix: Path
    readme: Path
    deviations_heading: str
    deviations_header: str
    pin: Mapping[tuple[str, str], int | None]
    pin_name: str
    excludable: frozenset[str]
    counts_columns: tuple[str, ...]
    annotated: tuple[Path, ...] = ()

    @property
    def functions(self) -> Counter[str]:
        """How many pinned functions each upstream file defines."""
        return Counter(file for file, _ in self.pin)


#: The Python SDK, ported as ``crates/sdk``.
SDK = Upstream(
    name="typesafe-sdk-python",
    matrix=Path("docs/port-test-matrix.md"),
    readme=Path("README.md"),
    deviations_heading="## Deviations from the Python SDK",
    deviations_header="Python SDK",
    pin=UPSTREAM_TESTS,
    pin_name="UPSTREAM_TESTS",
    excludable=EXCLUDED_FILES,
    counts_columns=("functions", "rust", "deviation", "excluded"),
)
#: Every ``test_*`` function the Python adapter's upstream files define at the
#: ported release (system-one-adapter-python e1d4cc9, v0.2.1), as
#: ``(file, name) -> cases``: the number of cases ``uv run --offline
#: --all-extras --all-groups pytest --collect-only -q`` collects for it. No
#: function is excluded, so no entry is ``None``.
ADAPTER_TESTS: Mapping[tuple[str, str], int | None] = MappingProxyType(
    {
        (
            "tests/test_client_with_fake_model.py",
            "test_attempts_are_independent_and_replayable",
        ): 2,
        (
            "tests/test_client_with_fake_model.py",
            "test_invalid_questions_are_rejected",
        ): 5,
        (
            "tests/test_client_with_fake_model.py",
            "test_malformed_retry_exhaustion_preserves_debug",
        ): 8,
        (
            "tests/test_client_with_fake_model.py",
            "test_malformed_structure_is_retried",
        ): 8,
        (
            "tests/test_client_with_fake_model.py",
            "test_missing_provider_setting_is_rejected",
        ): 2,
        (
            "tests/test_client_with_fake_model.py",
            "test_prompted_mode_adds_schema_instructions_native_does_not",
        ): 2,
        ("tests/test_client_with_fake_model.py", "test_retries_are_exhausted"): 2,
        (
            "tests/test_client_with_fake_model.py",
            "test_sdk_questions_and_response_serialization",
        ): 4,
        (
            "tests/test_client_with_fake_model.py",
            "test_structured_state_prompt_is_delimited_and_escapes_embedded_tags",
        ): 1,
        (
            "tests/test_client_with_fake_model.py",
            "test_transient_errors_are_retried",
        ): 4,
        (
            "tests/test_client_with_fake_model.py",
            "test_usage_separates_last_attempt_from_cumulative_totals",
        ): 2,
        (
            "tests/test_client_with_fake_model.py",
            "test_usage_totals_preserve_unknown_counts_across_corrections",
        ): 14,
        (
            "tests/test_client_with_live_apis.py",
            "test_live_models_follow_question_instructions_and_criteria",
        ): 12,
        (
            "tests/test_client_with_live_apis.py",
            "test_live_responses_match_reference_shape",
        ): 12,
        (
            "tests/test_client_with_live_apis.py",
            "test_live_typesafe_response_matches_reference_shape",
        ): 1,
        (
            "tests/test_gemini_transports.py",
            "test_gemini_incomplete_http_response_is_not_an_answer",
        ): 2,
        (
            "tests/test_gemini_transports.py",
            "test_gemini_transport_errors_obey_retry_budget",
        ): 8,
        (
            "tests/test_gemini_transports.py",
            "test_gemini_transport_preserves_corrections_and_usage",
        ): 4,
        (
            "tests/test_openai_transports.py",
            "test_concurrent_attempts_are_isolated_and_preserve_failed_responses",
        ): 3,
        (
            "tests/test_openai_transports.py",
            "test_custom_endpoint_from_environment_defaults_to_chat",
        ): 2,
        (
            "tests/test_openai_transports.py",
            "test_openai_transport_preserves_corrections_and_usage",
        ): 16,
        (
            "tests/test_openai_transports.py",
            "test_unfinished_responses_are_not_treated_as_answers",
        ): 2,
        (
            "tests/test_provider_lifecycle.py",
            "test_async_cleanup_propagates_cancellation_after_remaining_cleanup",
        ): 3,
        (
            "tests/test_provider_lifecycle.py",
            "test_cache_uses_resolved_provider_and_model_and_is_per_client",
        ): 4,
        (
            "tests/test_provider_lifecycle.py",
            "test_cancelling_close_waiter_does_not_interrupt_cleanup",
        ): 1,
        ("tests/test_provider_lifecycle.py", "test_cleanup_continues_after_failure"): 4,
        (
            "tests/test_provider_lifecycle.py",
            "test_close_before_first_use_does_not_construct_providers",
        ): 4,
        (
            "tests/test_provider_lifecycle.py",
            "test_concurrent_close_waits_for_same_cleanup",
        ): 8,
        (
            "tests/test_provider_lifecycle.py",
            "test_concurrent_first_use_reuses_pool_and_isolates_traces",
        ): 4,
        (
            "tests/test_provider_lifecycle.py",
            "test_custom_provider_without_close_remains_supported",
        ): 4,
        (
            "tests/test_provider_lifecycle.py",
            "test_environment_is_captured_on_first_use",
        ): 4,
        (
            "tests/test_provider_lifecycle.py",
            "test_exceptional_exit_closes_owned_sdks",
        ): 16,
        (
            "tests/test_provider_lifecycle.py",
            "test_failed_construction_is_not_cached",
        ): 4,
        ("tests/test_provider_lifecycle.py", "test_injected_provider_is_borrowed"): 8,
        (
            "tests/test_provider_lifecycle.py",
            "test_reuses_owned_provider_and_closes_sdk_on_context_exit",
        ): 4,
        ("tests/test_provider_nonanswers.py", "test_anthropic_nonanswers"): 36,
        ("tests/test_provider_nonanswers.py", "test_chat_completion_finish_reason"): 28,
        ("tests/test_provider_nonanswers.py", "test_openai_missing_usage"): 64,
        ("tests/test_provider_nonanswers.py", "test_openai_responses_refusal"): 8,
        ("tests/test_provider_requests.py", "test_anthropic_output_limit"): 8,
        (
            "tests/test_provider_requests.py",
            "test_anthropic_rejects_nonpositive_output_limit",
        ): 4,
        (
            "tests/test_provider_requests.py",
            "test_anthropic_request_omits_output_config_when_prompted",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_anthropic_request_puts_schema_in_output_config_when_structured",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_anthropic_result_joins_text_blocks_and_reads_usage",
        ): 1,
        ("tests/test_provider_requests.py", "test_build_providers_select_gemini"): 1,
        (
            "tests/test_provider_requests.py",
            "test_gemini_incomplete_status_is_not_treated_as_an_answer",
        ): 4,
        (
            "tests/test_provider_requests.py",
            "test_gemini_omitted_usage_is_not_treated_as_an_answer",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_gemini_request_omits_response_format_when_prompted",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_gemini_request_puts_schema_in_response_format_when_structured",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_gemini_request_sends_correction_turns_as_steps",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_gemini_result_reads_output_text_and_usage",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_openai_native_response_format_wraps_schema",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_openai_prompted_sends_no_response_format",
        ): 1,
        (
            "tests/test_provider_requests.py",
            "test_openai_result_reads_content_and_usage",
        ): 1,
        ("tests/test_provider_requests.py", "test_unknown_provider_is_rejected"): 1,
        (
            "tests/test_provider_retries.py",
            "test_retry_policy_controls_http_attempts",
        ): 12,
        ("tests/test_schema.py", "test_invalid_dictionary_questions_are_rejected"): 4,
        (
            "tests/test_schema.py",
            "test_output_rejects_extra_fields_and_internal_field_names",
        ): 3,
        (
            "tests/test_schema.py",
            "test_output_validation_preserves_types_bounds_and_allowed_values",
        ): 13,
        ("tests/test_schema.py", "test_probability_labels_preserve_arbitrary_names"): 1,
        ("tests/test_schema.py", "test_question_ids_preserve_arbitrary_names"): 2,
        ("tests/test_schema.py", "test_sdk_question_fields_are_revalidated"): 1,
        ("tests/utils/test_confidence_metrics.py", "test_confidence_metrics"): 8,
        (
            "tests/utils/test_error_handling.py",
            "test_non_retryable_error_is_not_retried",
        ): 1,
        (
            "tests/utils/test_error_handling.py",
            "test_retries_are_exhausted_and_reasons_recorded",
        ): 1,
        (
            "tests/utils/test_error_handling.py",
            "test_retries_succeed_after_transient_error",
        ): 1,
        (
            "tests/utils/test_error_handling.py",
            "test_status_errors_map_and_preserve_status_and_body",
        ): 18,
        (
            "tests/utils/test_error_handling.py",
            "test_timeout_and_connection_errors_map",
        ): 3,
        (
            "tests/utils/test_error_handling.py",
            "test_translating_context_manager_reraises_translated_error",
        ): 1,
        (
            "tests/utils/test_error_handling.py",
            "test_unknown_and_sdk_errors_pass_through",
        ): 3,
        (
            "tests/utils/test_probability_normalization.py",
            "test_probability_normalization_and_debug_data",
        ): 3,
    }
)
#: The Python adapter, ported as ``crates/adapter``.
ADAPTER = Upstream(
    name="system-one-adapter-python",
    matrix=Path("docs/adapter-port-test-matrix.md"),
    readme=Path("crates/adapter/README.md"),
    deviations_heading="## Deviations from the Python adapter",
    deviations_header="Deviation",
    pin=ADAPTER_TESTS,
    pin_name="ADAPTER_TESTS",
    excludable=frozenset(),
    counts_columns=("functions", "cases", "rust", "deviation"),
    annotated=(Path("crates/adapter"), Path("crates/adapter-live-tests")),
)
#: Every upstream whose matrix the checker runs over.
UPSTREAMS: tuple[Upstream, ...] = (SDK, ADAPTER)


@dataclass
class Row:
    """One upstream test and what covers it."""

    file: str
    line: int
    name: str
    cases: str
    target: str


@dataclass
class Counts:
    """The numbers a matrix states, or its rows give, for one upstream file."""

    functions: int = 0
    cases: int = 0
    rust: int = 0
    deviation: int = 0
    excluded: int = 0


@dataclass
class Matrix:
    """Everything read from one matrix file."""

    counts: dict[str, Counts] = field(default_factory=dict)
    rows: list[Row] = field(default_factory=list)


def table_cells(line: str) -> list[str] | None:
    """The cells of a Markdown table row, or ``None`` for any other line.

    Args:
        line: One line of Markdown.

    Returns:
        The stripped cells, or ``None`` when the line is not a data row (the
        separator row included).
    """
    stripped = line.strip()
    if not (stripped.startswith("|") and stripped.endswith("|")):
        return None
    cells = [cell.strip() for cell in stripped[1:-1].split("|")]
    if all(set(cell) <= set("-: ") for cell in cells):
        return None
    return cells


def read_matrix(text: str, counts_columns: tuple[str, ...]) -> Matrix:
    """Parse a matrix.

    Args:
        text: The contents of the matrix file.
        counts_columns: The number columns of its Counts table, in order.

    Returns:
        The stated counts and every row.
    """
    matrix = Matrix()
    section: str | None = None
    for number, line in enumerate(text.split("\n"), start=1):
        if line.startswith("## "):
            match = SECTION.match(line)
            section = match.group(1) if match else line[3:].strip()
            continue
        cells = table_cells(line)
        if cells is None or section is None:
            continue
        if section == "Counts" and cells[0].startswith("`tests/"):
            numbers = (int(cell) for cell in cells[1 : 1 + len(counts_columns)])
            matrix.counts[cells[0].strip("`")] = Counts(
                **dict(zip(counts_columns, numbers, strict=False))
            )
        elif section.startswith("Excluded") and cells[0].startswith("`tests/"):
            reason = EXCLUDED_PREFIX + (cells[2] if len(cells) > 2 else "")
            matrix.rows.append(
                Row(cells[0].strip("`"), number, cells[1].strip("`"), "", reason)
            )
        elif section.startswith("tests/") and cells[0].startswith("`test_"):
            target = cells[2] if len(cells) > 2 else ""
            matrix.rows.append(
                Row(section, number, cells[0].strip("`"), cells[1], target)
            )
    return matrix


def deviation_rows(readme: str, upstream: Upstream) -> set[str]:
    """The first cells of a README's deviations table.

    Args:
        readme: The contents of the README.
        upstream: The upstream, which names the table's heading and header.

    Returns:
        Every first cell of that table, header excluded.
    """
    _, found, rest = readme.partition(f"\n{upstream.deviations_heading}\n")
    if not found:
        return set()
    firsts: set[str] = set()
    for line in rest.split("\n"):
        if line.startswith("## "):
            break
        cells = table_cells(line)
        if cells is not None:
            firsts.add(cells[0])
    firsts.discard(upstream.deviations_header)
    return firsts


def defines_test(path: Path, name: str) -> bool:
    """Whether ``path`` (a file, or a directory searched recursively) has a test
    function called ``name``.

    A function counts when an attribute such as ``#[test]`` or
    ``#[tokio::test]`` opens a line of the block directly above it, which ends
    at a blank line or at the end of the previous item, so a helper of the same
    name does not.

    Args:
        path: A Rust source file or a directory of them.
        name: The function name.

    Returns:
        True when such a test exists.
    """
    files = sorted(path.rglob("*.rs")) if path.is_dir() else [path]
    signature = re.compile(
        rf"^\s*(?:pub(?:\([\w:]+\))?\s+)?(?:async\s+)?fn\s+{name}\s*[(<]"
    )
    for file in files:
        lines = file.read_text(encoding="utf-8").split("\n")
        for index, line in enumerate(lines):
            if not signature.match(line):
                continue
            above = index - 1
            while above >= 0:
                text = lines[above].strip()
                if not text or text.endswith(("}", ";")):
                    break
                if TEST_ATTRIBUTE.match(text):
                    return True
                above -= 1
    return False


def check_excluded(row: Row, upstream: Upstream) -> list[str]:
    """Check one excluded row.

    Args:
        row: The row; its target starts with ``EXCLUDED_PREFIX``.
        upstream: The upstream the row belongs to.

    Returns:
        One message per fault.
    """
    where = f"{upstream.matrix}:{row.line}: {row.file}::{row.name}"
    faults: list[str] = []
    if not upstream.excludable:
        faults.append(
            f"{where}: excluded, but no file of {upstream.name} may be excluded"
        )
    elif row.file not in upstream.excludable:
        faults.append(
            f"{where}: excluded, but {row.file} is not a tooling file "
            f"({', '.join(sorted(upstream.excludable))})"
        )
    if not row.target.removeprefix(EXCLUDED_PREFIX).strip():
        faults.append(f"{where}: excluded without a reason")
    if row.cases:
        faults.append(
            f"{where}: excluded, but its Cases cell holds {row.cases!r}; "
            "an excluded row has none"
        )
    return faults


def check_row(
    row: Row, upstream: Upstream, deviations: set[str], compare_cases: bool
) -> tuple[str | None, list[str]]:
    """Check one row's Cases and target.

    Args:
        row: The row.
        upstream: The upstream the row belongs to.
        deviations: The first cells of the README's deviations table.
        compare_cases: Whether to hold the row's Cases to the pin; false when
            the pin does not name the row's test, or the matrix names it twice,
            which are faults of their own.

    Returns:
        The row's kind (``"rust"``, ``"deviation"`` or ``"excluded"``,
        ``None`` when it is none of them) and one message per fault.
    """
    if row.target.startswith(EXCLUDED_PREFIX):
        return "excluded", check_excluded(row, upstream)
    where = f"{upstream.matrix}:{row.line}: {row.file}::{row.name}"
    faults: list[str] = []
    if not row.cases.isdigit() or int(row.cases) < 1:
        faults.append(f"{where}: the case count {row.cases!r} is not a positive number")
    elif compare_cases:
        pinned = upstream.pin[(row.file, row.name)]
        if pinned is None:
            faults.append(
                f"{where}: the case count is {row.cases}, but {upstream.pin_name} "
                "pins this test as excluded"
            )
        elif int(row.cases) != pinned:
            faults.append(
                f"{where}: the case count is {row.cases}, but "
                f"{upstream.pin_name} pins {pinned}"
            )
    if not row.target:
        return None, [*faults, f"{where}: no target"]

    deviation = DEVIATION_TARGET.match(row.target)
    if deviation:
        if deviation.group(1) not in deviations:
            faults.append(
                f"{where}: {deviation.group(1)!r} is not the first cell of a row of "
                f"{upstream.readme}'s deviations table"
            )
        return "deviation", faults

    targets = RUST_TARGET.findall(row.target)
    leftover = RUST_TARGET.sub("", row.target).replace(",", "").strip()
    if not targets or leftover:
        return None, [
            *faults,
            f"{where}: {row.target!r} is neither Rust tests nor a deviation",
        ]
    for path_text, name in targets:
        path = Path(path_text)
        if not path.exists():
            faults.append(f"{where}: {path_text} does not exist")
        elif not defines_test(path, name):
            faults.append(f"{where}: {path_text} has no test function `{name}`")
    return "rust", faults


def check_names(matrix: Matrix, upstream: Upstream) -> list[str]:
    """Compare the rows' ``(file, name)`` pairs with the upstream's pin.

    Every pinned upstream function must have a row, and every row must name a
    pinned function; a row that renames one, or swaps it for a made-up name,
    fails both ways. A second row for one function is reported by
    :func:`check_upstream_matrix`.

    Args:
        matrix: The parsed matrix.
        upstream: The upstream whose pin the rows are held to.

    Returns:
        One message per missing and per unknown pair, sorted.
    """
    named = {(row.file, row.name) for row in matrix.rows}
    pinned = upstream.pin.keys()
    faults = [
        f"{upstream.matrix}: upstream {file}::{name} has no row"
        for file, name in pinned - named
    ]
    faults.extend(
        f"{upstream.matrix}: {file}::{name} is not an upstream test"
        for file, name in named - pinned
    )
    return sorted(faults)


def check_checkout(upstream: Upstream, checkout: Path) -> list[str]:
    """Compare an upstream's pin with the functions a checkout defines.

    :func:`check_names` holds the rows to the pin in both forms; this holds the
    pin to upstream, so that the rows are checked against upstream through it.

    Args:
        upstream: The upstream whose pin is checked.
        checkout: A checkout of that upstream repository.

    Returns:
        One message per fault.
    """
    files = sorted(
        path.relative_to(checkout).as_posix()
        for pattern in UPSTREAM_GLOBS
        for path in checkout.glob(pattern)
    )
    if not files:
        return [f"{checkout}: no tests/test_*.py found"]
    defined = {
        (file, name)
        for file in files
        for name in UPSTREAM_TEST.findall((checkout / file).read_text(encoding="utf-8"))
    }
    pinned = upstream.pin.keys()
    # A pinned file that lost its functions is reported once per pinned test
    # below; only an unpinned one would otherwise pass without a word.
    faults = [
        f"{checkout}: {file} defines no top-level test_* function; "
        "the pin cannot cover it"
        for file in sorted(
            set(files) - {file for file, _ in defined} - upstream.functions.keys()
        )
    ]
    faults.extend(
        f"{checkout}: {file}::{name} is defined upstream but not in {upstream.pin_name}"
        for file, name in defined - pinned
    )
    faults.extend(
        f"{checkout}: {file}::{name} is in {upstream.pin_name} but not defined upstream"
        for file, name in pinned - defined
    )
    return sorted(faults)


@dataclass(frozen=True)
class Annotation:
    """One ``// Upstream:`` line of a Rust source.

    Attributes:
        path: The Rust file.
        line: The line's number.
        text: The line, stripped.
        pair: The upstream file and function it names, or ``None`` when the
            line is not of the form ``// Upstream: <file>::<function>``.
        test: The name and the line of the test function whose block of
            attributes and comments holds it, or ``None`` when no test's does.
    """

    path: Path
    line: int
    text: str
    pair: tuple[str, str] | None
    test: tuple[str, int] | None


def read_annotations(path: Path) -> tuple[list[Annotation], dict[str, list[int]]]:
    """The ``// Upstream:`` lines of a Rust file, and its test functions.

    A function's block is found as :func:`defines_test` finds it: the lines
    directly above it, up to a blank line or the end of the previous item.
    A comment counts as a test's only inside such a block that holds a test
    attribute.

    Args:
        path: A Rust source file.

    Returns:
        Every ``// Upstream:`` line, and the lines of the test functions by
        name.
    """
    lines = path.read_text(encoding="utf-8").split("\n")
    holder: dict[int, tuple[str, int]] = {}
    tests: dict[str, list[int]] = {}
    for index, line in enumerate(lines):
        function = FUNCTION.match(line)
        if not function:
            continue
        block: list[int] = []
        is_test = False
        above = index - 1
        while above >= 0:
            text = lines[above].strip()
            if not text or text.endswith(("}", ";")):
                break
            is_test = is_test or bool(TEST_ATTRIBUTE.match(text))
            block.append(above)
            above -= 1
        if is_test:
            name = function.group(1)
            tests.setdefault(name, []).append(index + 1)
            holder.update(dict.fromkeys(block, (name, index + 1)))
    annotations = []
    for index, line in enumerate(lines):
        if not UPSTREAM_COMMENT.match(line):
            continue
        named = UPSTREAM_LINE.match(line)
        annotations.append(
            Annotation(
                path,
                index + 1,
                line.strip(),
                (named.group(1), named.group(2)) if named else None,
                holder.get(index),
            )
        )
    return annotations, tests


def within(path: Path, place: Path) -> bool:
    """Whether ``path`` is ``place`` or lies under it.

    Args:
        path: A file or directory, relative to the repository root.
        place: A file or directory, relative to the repository root.

    Returns:
        True when ``path`` is ``place`` or one of its descendants.
    """
    return path == place or place in path.parents


def check_annotations(matrix: Matrix, upstream: Upstream) -> list[str]:
    """Hold the ``// Upstream:`` lines under the annotated directories to the rows.

    Every such line must name a row's function and sit directly above a test
    that row lists, and every test a row lists under those directories must
    carry a line naming the row's function.

    Args:
        matrix: The parsed matrix.
        upstream: The upstream; its ``annotated`` directories are read.

    Returns:
        One message per fault: the lines' faults by file and line, then the
        rows' missing lines in the order of the rows.
    """
    files = sorted(
        {
            file
            for place in upstream.annotated
            for file in (place.rglob("*.rs") if place.is_dir() else [place])
        }
    )
    annotations: list[Annotation] = []
    tests: dict[Path, dict[str, list[int]]] = {}
    for file in files:
        found, defined = read_annotations(file)
        annotations.extend(found)
        tests[file] = defined

    rows: dict[tuple[str, str], Row] = {}
    for row in matrix.rows:
        rows.setdefault((row.file, row.name), row)
    faults: list[str] = []
    carried: dict[tuple[Path, int], set[tuple[str, str]]] = {}
    for annotation in annotations:
        where = f"{annotation.path}:{annotation.line}"
        if annotation.pair is None:
            faults.append(
                f"{where}: {annotation.text!r} is not `// Upstream: <file>::<function>`"
            )
            continue
        if annotation.test is None:
            faults.append(f"{where}: {annotation.text} is not directly above a test")
            continue
        name, line = annotation.test
        carried.setdefault((annotation.path, line), set()).add(annotation.pair)
        row = rows.get(annotation.pair)
        if row is None:
            faults.append(
                f"{where}: {annotation.text} names no row of {upstream.matrix}"
            )
        elif not any(
            target == name and within(annotation.path, Path(path))
            for path, target in RUST_TARGET.findall(row.target)
        ):
            faults.append(
                f"{where}: {annotation.text} is above `{name}`, which the row at "
                f"{upstream.matrix}:{row.line} does not list"
            )

    for row in matrix.rows:
        for path_text, name in RUST_TARGET.findall(row.target):
            target = Path(path_text)
            if not any(within(target, place) for place in upstream.annotated):
                continue
            for file in files:
                if not within(file, target):
                    continue
                for line in tests[file].get(name, []):
                    if (row.file, row.name) not in carried.get((file, line), set()):
                        faults.append(
                            f"{upstream.matrix}:{row.line}: {row.file}::{row.name}: "
                            f"`{name}` at {file}:{line} carries no "
                            f"`// Upstream: {row.file}::{row.name}` line"
                        )
    return faults


def check_upstream_matrix(upstream: Upstream) -> tuple[list[str], str]:
    """Check one upstream's matrix.

    Args:
        upstream: The upstream whose matrix is checked.

    Returns:
        One message per fault, and the summary of the rows.
    """
    matrix = read_matrix(
        upstream.matrix.read_text(encoding="utf-8"), upstream.counts_columns
    )
    deviations = deviation_rows(upstream.readme.read_text(encoding="utf-8"), upstream)
    faults: list[str] = []
    if not deviations:
        faults.append(f"{upstream.readme}: no {upstream.deviations_heading!r} table")
    if not matrix.counts:
        faults.append(f"{upstream.matrix}: no Counts table")

    occurrences = Counter((row.file, row.name) for row in matrix.rows)
    tally: dict[str, Counts] = {}
    seen: set[tuple[str, str]] = set()
    for row in matrix.rows:
        key = (row.file, row.name)
        if key in seen:
            faults.append(
                f"{upstream.matrix}:{row.line}: {row.file}::{row.name} has a second row"
            )
        seen.add(key)
        compare_cases = key in upstream.pin and occurrences[key] == 1
        kind, row_faults = check_row(row, upstream, deviations, compare_cases)
        faults.extend(row_faults)
        counts = tally.setdefault(row.file, Counts())
        counts.functions += 1
        counts.cases += int(row.cases) if row.cases.isdigit() else 0
        counts.rust += kind == "rust"
        counts.deviation += kind == "deviation"
        counts.excluded += kind == "excluded"

    columns = upstream.counts_columns
    for file in sorted(set(matrix.counts) | set(tally)):
        stated, counted = matrix.counts.get(file), tally.get(file, Counts())
        if stated is None:
            faults.append(
                f"{upstream.matrix}: {file} has rows but no line in the Counts table"
            )
        elif any(
            getattr(stated, column) != getattr(counted, column) for column in columns
        ):
            states = " / ".join(
                f"{getattr(stated, column)} {COUNTS_LABELS[column]}"
                for column in columns
            )
            gives = " / ".join(str(getattr(counted, column)) for column in columns)
            faults.append(
                f"{upstream.matrix}: {file} states {states}, the rows give {gives}"
            )

    faults.extend(
        f"{upstream.matrix}: {file} is not an upstream test file"
        for file in sorted(matrix.counts.keys() - upstream.functions.keys())
    )
    faults.extend(check_names(matrix, upstream))
    if upstream.annotated:
        faults.extend(check_annotations(matrix, upstream))

    rust = sum(counts.rust for counts in tally.values())
    deviation = sum(counts.deviation for counts in tally.values())
    excluded = sum(counts.excluded for counts in tally.values())
    summary = (
        f"{len(matrix.rows)} rows for {len(upstream.pin)} upstream tests in "
        f"{len(upstream.functions)} files: {rust} to Rust tests, {deviation} to "
        f"deviations, {excluded} excluded as Python tooling"
    )
    return faults, summary


def main(argv: list[str]) -> int:
    """Check every upstream's matrix, and with ``--upstream`` its pin.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        The process exit status: 0 when every check passes.
    """
    names = [upstream.name for upstream in UPSTREAMS]
    parser = argparse.ArgumentParser(
        description="Check the upstream test matrix of every port."
    )
    parser.add_argument(
        "--upstream",
        action="append",
        default=[],
        metavar="NAME=CHECKOUT",
        help=f"a checkout of the upstream NAME ({', '.join(names)}); repeatable",
    )
    arguments = parser.parse_args(argv)
    checkouts: dict[str, Path] = {}
    for value in arguments.upstream:
        name, equals, checkout = value.partition("=")
        if not equals or not checkout:
            parser.error(f"--upstream {value!r} is not NAME=CHECKOUT")
        if name not in names:
            parser.error(
                f"--upstream {value!r}: {name!r} is not one of {', '.join(names)}"
            )
        if name in checkouts:
            parser.error(f"--upstream names {name!r} twice")
        checkouts[name] = Path(checkout)

    faults: list[str] = []
    summaries: list[str] = []
    for upstream in UPSTREAMS:
        upstream_faults, summary = check_upstream_matrix(upstream)
        faults.extend(upstream_faults)
        if upstream.name in checkouts:
            faults.extend(check_checkout(upstream, checkouts[upstream.name]))
        # One upstream keeps the summary the matrix has always printed; with
        # more, each line says which upstream it counts.
        summaries.append(
            summary if len(UPSTREAMS) == 1 else f"{upstream.name}: {summary}"
        )

    for fault in faults:
        print(fault)
    summary = "\n".join(summaries)
    if faults:
        print(f"\n{len(faults)} fault(s); {summary}")
        return 1
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
