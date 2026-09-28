#!/usr/bin/env python3
"""Check that docs/port-test-matrix.md maps every upstream test to something real.

The matrix names, for each test function of the Python SDK's test suite, the
Rust tests that cover its behaviour or the row of the README's deviations table
that explains why there is none. A mapping that names a test which was renamed
or deleted, or a deviation row that was reworded, still reads as a mapping, so
nothing but a check notices that it has stopped being one. This is that check.

It fails on:

* a row whose target cell is empty, or holds neither Rust tests nor a
  deviation, an excluded row without a reason, and an excluded row of a file
  that is not one of the tooling files in ``EXCLUDED_FILES``;
* a Rust target ``path::name`` when ``path`` (a file, or a directory searched
  recursively) does not exist or has no ``fn name`` carrying a test attribute;
* a quoted deviation that is not the first cell of a row of the deviations
  table in README.md;
* an upstream test (``UPSTREAM_TESTS``, the ``(file, name)`` of every upstream
  ``test_*`` function, pinned so that no upstream checkout is needed) without a
  row, a row naming a pair that is not in it, and an upstream test named twice;
* per-file counts that differ from the ones the matrix states, and stated
  function counts that differ from upstream's (derived from ``UPSTREAM_TESTS``).

A row is one of three kinds: mapped to Rust tests, mapped to a deviation row,
or excluded with a reason (only the functions of the upstream files that test
the Python repository's own tooling).

With ``--upstream <checkout>`` it also checks ``UPSTREAM_TESTS`` itself: every
``test_*`` function the checkout defines is pinned, and every pinned one is
defined. It also fails on:

* an upstream ``tests/test_*.py`` the pin does not name that defines no
  module-level ``test_*`` function (none at all, or only methods of a class),
  which the pin cannot cover.

Run it from the repository root.
"""

import argparse
import re
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

MATRIX = Path("docs/port-test-matrix.md")
README = Path("README.md")
DEVIATIONS_HEADING = "## Deviations from the Python SDK"

#: A section of the matrix: ``## `tests/test_x.py` ``.
SECTION = re.compile(r"^## `(tests/test_\w+\.py)`\s*$")
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
#: The upstream files that test the Python repository's own tooling, the only
#: ones whose functions may be excluded.
EXCLUDED_FILES = frozenset(
    {
        "tests/test_docs.py",
        "tests/test_public_api_surface.py",
        "tests/test_public_sync.py",
        "tests/test_release_notes.py",
        "tests/test_typing.py",
    }
)
#: Every ``test_*`` function the upstream files define at the ported release
#: (typesafe-sdk-python f078f1e, v0.7.2), as ``(file, name)``. Pinned so that a
#: dropped, renamed or made-up row fails without an upstream checkout;
#: ``--upstream`` checks the pin itself against a checkout.
UPSTREAM_TESTS = frozenset(
    {
        ("tests/test_clients.py", "test_cancellation_propagates"),
        ("tests/test_clients.py", "test_error_mapping"),
        ("tests/test_clients.py", "test_error_messages"),
        ("tests/test_clients.py", "test_exceptional_context_closes_http_client"),
        ("tests/test_clients.py", "test_extra_body_shallow_override"),
        ("tests/test_clients.py", "test_headers_timeout_and_logging"),
        ("tests/test_clients.py", "test_http_client_settings"),
        ("tests/test_clients.py", "test_invalid_models_response"),
        ("tests/test_clients.py", "test_models_ignore_unknown_fields"),
        ("tests/test_clients.py", "test_models_shape"),
        ("tests/test_clients.py", "test_owned_http_client_closed"),
        ("tests/test_clients.py", "test_question_schema_validation_is_left_to_api"),
        ("tests/test_clients.py", "test_raw_question_passthrough"),
        ("tests/test_clients.py", "test_rich_descriptions"),
        ("tests/test_clients.py", "test_round_trip"),
        ("tests/test_clients.py", "test_supplied_network_resources_closed"),
        ("tests/test_clients.py", "test_system_one_timeout_override"),
        ("tests/test_clients.py", "test_task_cancellation_closes_context"),
        ("tests/test_clients.py", "test_transport_errors"),
        ("tests/test_clients.py", "test_unserializable_request_body_raises"),
        ("tests/test_clients.py", "test_validation_before_network"),
        ("tests/test_config.py", "test_api_key_whitespace"),
        ("tests/test_config.py", "test_empty_env_unset"),
        ("tests/test_config.py", "test_http_client_timeout_precedence"),
        ("tests/test_config.py", "test_invalid_api_key"),
        ("tests/test_config.py", "test_invalid_explicit_key_does_not_fall_back_to_env"),
        ("tests/test_config.py", "test_invalid_timeout"),
        ("tests/test_config.py", "test_missing_key"),
        ("tests/test_config.py", "test_model_override"),
        ("tests/test_config.py", "test_resolution"),
        ("tests/test_config.py", "test_timeout_object"),
        ("tests/test_config.py", "test_transport_and_http_client_mutually_exclusive"),
        ("tests/test_docs.py", "test_markdown"),
        ("tests/test_docs.py", "test_python_doctests"),
        ("tests/test_errors.py", "test_api_error_endpoint_omits_url_credentials"),
        ("tests/test_errors.py", "test_api_error_from_process_pool"),
        ("tests/test_errors.py", "test_api_error_request_context"),
        ("tests/test_errors.py", "test_error_body_edge_cases"),
        ("tests/test_errors.py", "test_exception_reconstruction"),
        ("tests/test_errors.py", "test_message_override"),
        ("tests/test_integration.py", "test_live_models"),
        ("tests/test_integration.py", "test_live_pydantic_response"),
        ("tests/test_integration.py", "test_live_questions"),
        ("tests/test_logging.py", "test_exception_redaction_escaped_values"),
        (
            "tests/test_logging.py",
            "test_exception_redaction_preserves_network_diagnostics",
        ),
        (
            "tests/test_logging.py",
            "test_exception_redaction_shared_causes_cycles_and_notes",
        ),
        ("tests/test_logging.py", "test_exception_redaction_structured_constructor"),
        ("tests/test_logging.py", "test_logger_level_controls_output"),
        ("tests/test_logging.py", "test_secret_headers_redacted"),
        ("tests/test_logging.py", "test_setup_logging_from_env"),
        ("tests/test_logging.py", "test_transport_errors_do_not_expose_credentials"),
        ("tests/test_public_api_surface.py", "test_constructor_kwargs"),
        ("tests/test_public_api_surface.py", "test_package_exports"),
        ("tests/test_public_api_surface.py", "test_public_members"),
        ("tests/test_public_sync.py", "test_atomic_push_rejects_concurrent_update"),
        ("tests/test_public_sync.py", "test_dry_run_skips_github"),
        (
            "tests/test_public_sync.py",
            "test_existing_history_deletions_and_immutable_tags",
        ),
        ("tests/test_public_sync.py", "test_invalid_includes"),
        ("tests/test_public_sync.py", "test_release_contributors"),
        ("tests/test_public_sync.py", "test_sign_snapshot_and_push"),
        ("tests/test_public_sync.py", "test_signing_failure_keeps_refs"),
        ("tests/test_public_sync.py", "test_snapshot_and_push_retries"),
        ("tests/test_public_sync.py", "test_unsafe_snapshots"),
        ("tests/test_public_sync.py", "test_version_mismatch"),
        (
            "tests/test_pydantic_response_models.py",
            "test_custom_response_preserves_api_errors",
        ),
        (
            "tests/test_pydantic_response_models.py",
            "test_explicit_default_response_model",
        ),
        ("tests/test_pydantic_response_models.py", "test_pydantic_response_validation"),
        (
            "tests/test_pydantic_response_models.py",
            "test_pydantic_system_one_response_subclass",
        ),
        (
            "tests/test_pydantic_response_models.py",
            "test_standalone_pydantic_response_model",
        ),
        ("tests/test_questions.py", "test_covariant_question_mappings"),
        ("tests/test_questions.py", "test_direct_encoding_omits_only_default_fields"),
        ("tests/test_questions.py", "test_discriminators_are_automatic"),
        ("tests/test_questions.py", "test_empty_score_criteria_is_rejected"),
        (
            "tests/test_questions.py",
            "test_invalid_typed_question_is_rejected_on_construction",
        ),
        ("tests/test_questions.py", "test_normalization_preserves_objects"),
        ("tests/test_questions.py", "test_normalization_preserves_raw_questions"),
        ("tests/test_questions.py", "test_optional_noul_criteria"),
        ("tests/test_questions.py", "test_raw_questions_require_structural_keys"),
        ("tests/test_questions.py", "test_typed_noul_criteria_reject_unknown_fields"),
        ("tests/test_questions.py", "test_typed_questions_reject_unknown_fields"),
        ("tests/test_release_notes.py", "test_invalid_release_notes"),
        ("tests/test_release_notes.py", "test_release_notes"),
        ("tests/test_responses.py", "test_answer_attributes_and_dictionary_types"),
        ("tests/test_responses.py", "test_answer_fields_are_frozen"),
        ("tests/test_responses.py", "test_answer_groups_are_cached_and_not_serialized"),
        ("tests/test_responses.py", "test_copied_response_preserves_metadata"),
        ("tests/test_responses.py", "test_malformed_response_raises_validation_error"),
        ("tests/test_responses.py", "test_missing_raw_raises_on_access"),
        ("tests/test_responses.py", "test_missing_request_id_raises_on_access"),
        ("tests/test_responses.py", "test_nested_missing_field_path"),
        ("tests/test_responses.py", "test_public_response_types_ignore_unknown_fields"),
        ("tests/test_responses.py", "test_response_carries_raw_http_response"),
        ("tests/test_responses.py", "test_response_carries_request_id"),
        ("tests/test_responses.py", "test_response_preserves_nested_json"),
        (
            "tests/test_responses.py",
            "test_response_serialization_excludes_http_metadata",
        ),
        ("tests/test_responses.py", "test_unknown_answer_type_ignored"),
        ("tests/test_responses.py", "test_unknown_extra_fields_tolerated"),
        ("tests/test_retry.py", "test_async_concurrent_retry_state"),
        ("tests/test_retry.py", "test_backoff_dates_cap_and_jitter"),
        ("tests/test_retry.py", "test_backoff_extreme_values"),
        ("tests/test_retry.py", "test_cancel_pending_retry"),
        ("tests/test_retry.py", "test_concurrent_system_one_overrides"),
        ("tests/test_retry.py", "test_connection_retry_recovers"),
        ("tests/test_retry.py", "test_default_retry_statuses"),
        ("tests/test_retry.py", "test_exhausted_retry_preserves_final_http_error"),
        ("tests/test_retry.py", "test_exhausted_transport_retry"),
        ("tests/test_retry.py", "test_invalid_backoff"),
        ("tests/test_retry.py", "test_invalid_backoff_jitter"),
        ("tests/test_retry.py", "test_invalid_max_retries"),
        ("tests/test_retry.py", "test_parse_retry_after"),
        ("tests/test_retry.py", "test_retry_policy_custom_statuses"),
        ("tests/test_retry.py", "test_retry_policy_exceptions_and_predicate"),
        ("tests/test_retry.py", "test_retry_policy_invalid_timeout"),
        ("tests/test_retry.py", "test_retry_policy_max_retries"),
        ("tests/test_retry.py", "test_retry_policy_per_call_override"),
        ("tests/test_retry.py", "test_retry_policy_timeout_budget"),
        ("tests/test_retry.py", "test_retry_policy_timeout_override"),
        ("tests/test_retry.py", "test_retry_policy_wait_options"),
        ("tests/test_retry.py", "test_server_delay_through_tenacity"),
        ("tests/test_retry.py", "test_system_one_retry_override"),
        ("tests/test_retry.py", "test_system_one_retry_recovers_with_overrides"),
        ("tests/test_retry.py", "test_zero_backoff_retries"),
        ("tests/test_types.py", "test_abstract_input_containers_encode"),
        ("tests/test_types.py", "test_array_inputs"),
        ("tests/test_types.py", "test_explicitly_nullable_json_values"),
        ("tests/test_types.py", "test_json_value_and_state_exclude_top_level_none"),
        ("tests/test_types.py", "test_raw_optional_fields_preserve_explicit_null"),
        ("tests/test_types.py", "test_str_subclasses_fallback_to_strings"),
        ("tests/test_typing.py", "test_public_typing"),
    }
)
#: How many ``test_*`` functions each upstream file defines, derived from
#: ``UPSTREAM_TESTS`` so that the names are the one source of truth.
UPSTREAM_FUNCTIONS = Counter(file for file, _ in UPSTREAM_TESTS)


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
    """The numbers the matrix states for one upstream file."""

    functions: int
    rust: int
    deviation: int
    excluded: int


@dataclass
class Matrix:
    """Everything read from the matrix file."""

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


def read_matrix(text: str) -> Matrix:
    """Parse the matrix.

    Args:
        text: The contents of docs/port-test-matrix.md.

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
            matrix.counts[cells[0].strip("`")] = Counts(
                *(int(cell) for cell in cells[1:5])
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


def deviation_rows(readme: str) -> set[str]:
    """The first cells of the README's deviations table.

    Args:
        readme: The contents of README.md.

    Returns:
        Every first cell of that table, header excluded.
    """
    _, found, rest = readme.partition(f"\n{DEVIATIONS_HEADING}\n")
    if not found:
        return set()
    firsts: set[str] = set()
    for line in rest.split("\n"):
        if line.startswith("## "):
            break
        cells = table_cells(line)
        if cells is not None:
            firsts.add(cells[0])
    firsts.discard("Python SDK")
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


def check_row(row: Row, deviations: set[str]) -> tuple[str | None, list[str]]:
    """Check one row's target.

    Args:
        row: The row.
        deviations: The first cells of the README's deviations table.

    Returns:
        The row's kind (``"rust"``, ``"deviation"`` or ``"excluded"``,
        ``None`` when it is none of them) and one message per fault.
    """
    where = f"{MATRIX}:{row.line}: {row.file}::{row.name}"
    if row.target.startswith(EXCLUDED_PREFIX):
        excluded: list[str] = []
        if row.file not in EXCLUDED_FILES:
            excluded.append(
                f"{where}: excluded, but {row.file} is not a tooling file "
                f"({', '.join(sorted(EXCLUDED_FILES))})"
            )
        if not row.target.removeprefix(EXCLUDED_PREFIX).strip():
            excluded.append(f"{where}: excluded without a reason")
        return "excluded", excluded
    faults: list[str] = []
    if not row.cases.isdigit() or int(row.cases) < 1:
        faults.append(f"{where}: the case count {row.cases!r} is not a positive number")
    if not row.target:
        return None, [*faults, f"{where}: no target"]

    deviation = DEVIATION_TARGET.match(row.target)
    if deviation:
        if deviation.group(1) not in deviations:
            faults.append(
                f"{where}: {deviation.group(1)!r} is not the first cell of a row of "
                f"{README}'s deviations table"
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


def check_names(matrix: Matrix) -> list[str]:
    """Compare the rows' ``(file, name)`` pairs with ``UPSTREAM_TESTS``.

    Every pinned upstream function must have a row, and every row must name a
    pinned function; a row that renames one, or swaps it for a made-up name,
    fails both ways. A second row for one function is reported by :func:`main`.

    Args:
        matrix: The parsed matrix.

    Returns:
        One message per missing and per unknown pair, sorted.
    """
    named = {(row.file, row.name) for row in matrix.rows}
    faults = [
        f"{MATRIX}: upstream {file}::{name} has no row"
        for file, name in UPSTREAM_TESTS - named
    ]
    faults.extend(
        f"{MATRIX}: {file}::{name} is not an upstream test"
        for file, name in named - UPSTREAM_TESTS
    )
    return sorted(faults)


def check_upstream(upstream: Path) -> list[str]:
    """Compare ``UPSTREAM_TESTS`` with the functions a checkout defines.

    :func:`check_names` holds the rows to the pin in both forms; this holds the
    pin to upstream, so that the rows are checked against upstream through it.

    Args:
        upstream: A checkout of the upstream repository.

    Returns:
        One message per fault.
    """
    files = sorted(
        path.relative_to(upstream).as_posix()
        for path in upstream.glob("tests/test_*.py")
    )
    if not files:
        return [f"{upstream}: no tests/test_*.py found"]
    defined = {
        (file, name)
        for file in files
        for name in UPSTREAM_TEST.findall((upstream / file).read_text(encoding="utf-8"))
    }
    # A pinned file that lost its functions is reported once per pinned test
    # below; only an unpinned one would otherwise pass without a word.
    faults = [
        f"{upstream}: {file} defines no top-level test_* function; "
        "the pin cannot cover it"
        for file in sorted(
            set(files) - {file for file, _ in defined} - UPSTREAM_FUNCTIONS.keys()
        )
    ]
    faults.extend(
        f"{upstream}: {file}::{name} is defined upstream but not in UPSTREAM_TESTS"
        for file, name in defined - UPSTREAM_TESTS
    )
    faults.extend(
        f"{upstream}: {file}::{name} is in UPSTREAM_TESTS but not defined upstream"
        for file, name in UPSTREAM_TESTS - defined
    )
    return sorted(faults)


def main(argv: list[str]) -> int:
    """Check the matrix, and with ``--upstream`` compare it with upstream.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        The process exit status: 0 when every check passes.
    """
    parser = argparse.ArgumentParser(description="Check docs/port-test-matrix.md.")
    parser.add_argument(
        "--upstream", type=Path, help="a checkout of typesafe-sdk-python"
    )
    arguments = parser.parse_args(argv)

    matrix = read_matrix(MATRIX.read_text(encoding="utf-8"))
    deviations = deviation_rows(README.read_text(encoding="utf-8"))
    faults: list[str] = []
    if not deviations:
        faults.append(f"{README}: no {DEVIATIONS_HEADING!r} table")
    if not matrix.counts:
        faults.append(f"{MATRIX}: no Counts table")

    tally: dict[str, Counts] = {}
    seen: set[tuple[str, str]] = set()
    for row in matrix.rows:
        if (row.file, row.name) in seen:
            faults.append(
                f"{MATRIX}:{row.line}: {row.file}::{row.name} has a second row"
            )
        seen.add((row.file, row.name))
        kind, row_faults = check_row(row, deviations)
        faults.extend(row_faults)
        counts = tally.setdefault(row.file, Counts(0, 0, 0, 0))
        counts.functions += 1
        counts.rust += kind == "rust"
        counts.deviation += kind == "deviation"
        counts.excluded += kind == "excluded"

    for file in sorted(set(matrix.counts) | set(tally)):
        stated, counted = matrix.counts.get(file), tally.get(file, Counts(0, 0, 0, 0))
        if stated is None:
            faults.append(f"{MATRIX}: {file} has rows but no line in the Counts table")
        elif stated != counted:
            faults.append(
                f"{MATRIX}: {file} states {stated.functions} functions / {stated.rust} Rust / "
                f"{stated.deviation} deviation / {stated.excluded} excluded, the rows give "
                f"{counted.functions} / {counted.rust} / {counted.deviation} / "
                f"{counted.excluded}"
            )

    faults.extend(
        f"{MATRIX}: {file} is not an upstream test file"
        for file in sorted(matrix.counts.keys() - UPSTREAM_FUNCTIONS)
    )

    faults.extend(check_names(matrix))
    if arguments.upstream is not None:
        faults.extend(check_upstream(arguments.upstream))

    for fault in faults:
        print(fault)
    rust = sum(counts.rust for counts in tally.values())
    deviation = sum(counts.deviation for counts in tally.values())
    excluded = sum(counts.excluded for counts in tally.values())
    summary = (
        f"{len(matrix.rows)} rows for {len(UPSTREAM_TESTS)} upstream tests in "
        f"{len(UPSTREAM_FUNCTIONS)} files: {rust} to Rust tests, {deviation} to "
        f"deviations, {excluded} excluded as Python tooling"
    )
    if faults:
        print(f"\n{len(faults)} fault(s); {summary}")
        return 1
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
