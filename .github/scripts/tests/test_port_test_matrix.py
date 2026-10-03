"""Behaviour of ``port-test-matrix.py`` on the tracked page and on planted errors.

Every error is planted on a copy of the page or of the README under the test's
temporary directory; the tracked files are only read.
"""

from collections.abc import Callable, Mapping
from dataclasses import dataclass, replace
from functools import partial
from pathlib import Path
from types import MappingProxyType, ModuleType
from typing import Any

import pytest

MATRIX = "docs/port-test-matrix.md"
README = "README.md"
CLIENTS = "tests/test_clients.py"
CONFIG = "tests/test_config.py"
ERRORS = "tests/test_errors.py"
SDK_NAME = "typesafe-sdk-python"
TOOLING_FILES = (
    "tests/test_docs.py, tests/test_public_api_surface.py, tests/test_public_sync.py,"
    " tests/test_release_notes.py, tests/test_typing.py"
)


def summary(
    rows: int = 129,
    rust: int = 94,
    deviation: int = 17,
    excluded: int = 18,
    tests: int = 129,
    files: int = 15,
) -> str:
    """The checker's closing line for the given tallies.

    The defaults are the tallies of the tracked page.

    Args:
        rows: The rows the page holds.
        rust: The rows mapped to Rust tests.
        deviation: The rows mapped to a deviation.
        excluded: The excluded rows.
        tests: The upstream tests the pin names.
        files: The upstream files the pin names.

    Returns:
        The summary line, without its line feed.
    """
    return (
        f"{rows} rows for {tests} upstream tests in {files} files: {rust} to Rust tests, "
        f"{deviation} to deviations, {excluded} excluded as Python tooling"
    )


def failed(faults: list[str], closing: str) -> str:
    """What the checker prints when it finds faults.

    Args:
        faults: The fault lines, in the order they are printed.
        closing: The summary line.

    Returns:
        The whole standard output.
    """
    return (
        "".join(f"{fault}\n" for fault in faults)
        + f"\n{len(faults)} fault(s); {closing}\n"
    )


def line_of(text: str, needle: str) -> int:
    """The number of the only line holding ``needle``.

    Args:
        text: A whole file.
        needle: Text that occurs on exactly one line.

    Returns:
        That line's 1-based number.
    """
    numbers = [
        number
        for number, line in enumerate(text.split("\n"), start=1)
        if needle in line
    ]
    assert len(numbers) == 1, f"{needle!r} is on lines {numbers}"
    return numbers[0]


def replace_once(text: str, old: str, new: str) -> str:
    """Replace text that occurs exactly once.

    Args:
        text: A whole file.
        old: The text to replace; it must occur once.
        new: Its replacement.

    Returns:
        The edited file.
    """
    assert text.count(old) == 1, f"{old!r} occurs {text.count(old)} times"
    return text.replace(old, new)


def delete_line(text: str, needle: str) -> str:
    """Delete the only line holding ``needle``.

    Args:
        text: A whole file.
        needle: Text that occurs on exactly one line.

    Returns:
        The file without that line.
    """
    lines = text.split("\n")
    del lines[line_of(text, needle) - 1]
    return "\n".join(lines)


Run = Callable[..., tuple[int, str]]


@pytest.fixture
def run(
    port_test_matrix: ModuleType,
    repository: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> Run:
    """Run the checker on copies of the page and the README.

    The checker runs from the repository root, so that the Rust paths the
    rows name resolve to the tracked tests. Its output names the copies by
    the tracked paths, as it would name the tracked files.
    """
    matrix = tmp_path / "port-test-matrix.md"
    readme = tmp_path / "README.md"
    sdk = replace(port_test_matrix.SDK, matrix=matrix, readme=readme)
    monkeypatch.setattr(port_test_matrix, "UPSTREAMS", (sdk,))

    def invoke(
        edit_matrix: Callable[[str], str] = lambda text: text,
        edit_readme: Callable[[str], str] = lambda text: text,
        argv: tuple[str, ...] = (),
    ) -> tuple[int, str]:
        matrix.write_text(
            edit_matrix((repository / MATRIX).read_text(encoding="utf-8")),
            encoding="utf-8",
        )
        readme.write_text(
            edit_readme((repository / README).read_text(encoding="utf-8")),
            encoding="utf-8",
        )
        status = port_test_matrix.main(list(argv))
        out = capsys.readouterr().out
        return status, out.replace(str(matrix), MATRIX).replace(str(readme), README)

    return invoke


def pristine_matrix(repository: Path) -> str:
    """The tracked page.

    Args:
        repository: The repository root.

    Returns:
        The page's text.
    """
    return (repository / MATRIX).read_text(encoding="utf-8")


def test_tracked_page_passes(run: Run) -> None:
    """The tracked page passes in the form CI runs."""
    status, out = run()

    assert (status, out) == (0, summary() + "\n")


@pytest.mark.parametrize(
    ("old", "new"),
    [
        ("test_error_mapping", "test_error_mapper"),
        ("test_round_trip", "test_round_trip_all"),
    ],
    ids=["a renamed upstream test", "a made-up name with the same target"],
)
def test_unknown_row_name_fails(run: Run, old: str, new: str) -> None:
    """A row naming no upstream test leaves that upstream test without a row."""
    status, out = run(lambda text: replace_once(text, f"| `{old}` |", f"| `{new}` |"))

    assert status == 1
    assert out == failed(
        [
            f"{MATRIX}: tests/test_clients.py::{new} is not an upstream test",
            f"{MATRIX}: upstream tests/test_clients.py::{old} has no row",
        ],
        summary(),
    )


def test_rows_swapped_between_files_fail(run: Run) -> None:
    """Two rows swapped between two sections fail both ways, in both files."""
    config = "| `test_missing_key` |"
    errors = "| `test_message_override` |"

    def swap(text: str) -> str:
        return (
            replace_once(text, config, "SWAPPED")
            .replace(errors, config)
            .replace("SWAPPED", errors)
        )

    status, out = run(swap)

    assert status == 1
    assert out == failed(
        [
            f"{MATRIX}: {CONFIG}::test_message_override is not an upstream test",
            f"{MATRIX}: {ERRORS}::test_missing_key is not an upstream test",
            f"{MATRIX}: upstream {CONFIG}::test_missing_key has no row",
            f"{MATRIX}: upstream {ERRORS}::test_message_override has no row",
        ],
        summary(),
    )


def test_functional_row_moved_to_excluded_fails(run: Run, repository: Path) -> None:
    """Only the tooling files may have excluded rows."""
    excluded_row = "| `tests/test_clients.py` | `test_error_mapping` | not wanted |"
    header = "| Upstream file | Upstream test | Reason |\n| --- | --- | --- |\n"

    def move(text: str) -> str:
        text = delete_line(text, "| `test_error_mapping` |")
        text = replace_once(text, header, header + excluded_row + "\n")
        return replace_once(
            text,
            "| `tests/test_clients.py` | 21 | 21 | 0 | 0 |",
            "| `tests/test_clients.py` | 21 | 20 | 0 | 1 |",
        )

    line = line_of(move(pristine_matrix(repository)), excluded_row)

    status, out = run(move)

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}:{line}: {CLIENTS}::test_error_mapping: excluded, "
                f"but {CLIENTS} is not a tooling file ({TOOLING_FILES})"
            )
        ],
        summary(rust=93, excluded=19),
    )


def test_missing_path_fails(run: Run, repository: Path) -> None:
    """A Rust target whose path does not exist fails."""
    line = line_of(pristine_matrix(repository), "| `test_live_models` |")

    status, out = run(
        lambda text: replace_once(
            text, "`crates/live-tests::live_models`", "`crates/live-testz::live_models`"
        )
    )

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}:{line}: tests/test_integration.py::test_live_models: "
                "crates/live-testz does not exist"
            )
        ],
        summary(),
    )


def test_renamed_rust_test_fails(run: Run, repository: Path) -> None:
    """A Rust target naming no test function in an existing file fails."""
    line = line_of(pristine_matrix(repository), "| `test_error_mapping` |")

    status, out = run(
        lambda text: replace_once(
            text,
            "| `test_error_mapping` | 22 | `crates/sdk/tests/client.rs::error_mapping` |",
            "| `test_error_mapping` | 22 | `crates/sdk/tests/client.rs::error_mappingz` |",
        )
    )

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}:{line}: tests/test_clients.py::test_error_mapping: "
                "crates/sdk/tests/client.rs has no test function `error_mappingz`"
            )
        ],
        summary(),
    )


def test_emptied_target_fails(run: Run, repository: Path) -> None:
    """A row with no target fails, and no longer counts as a Rust row."""
    line = line_of(pristine_matrix(repository), "| `test_error_mapping` |")

    status, out = run(
        lambda text: replace_once(
            text,
            "| `test_error_mapping` | 22 | `crates/sdk/tests/client.rs::error_mapping` |",
            "| `test_error_mapping` | 22 |  |",
        )
    )

    assert status == 1
    assert out == failed(
        [
            f"{MATRIX}:{line}: tests/test_clients.py::test_error_mapping: no target",
            (
                f"{MATRIX}: tests/test_clients.py states 21 functions / 21 Rust / "
                "0 deviation / 0 excluded, the rows give 21 / 20 / 0 / 0"
            ),
        ],
        summary(rust=93),
    )


def test_deleted_row_fails(run: Run) -> None:
    """A deleted row fails the Counts table and the pin."""
    status, out = run(lambda text: delete_line(text, "| `test_error_mapping` |"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}: tests/test_clients.py states 21 functions / 21 Rust / "
                "0 deviation / 0 excluded, the rows give 20 / 20 / 0 / 0"
            ),
            f"{MATRIX}: upstream tests/test_clients.py::test_error_mapping has no row",
        ],
        summary(rows=128, rust=93),
    )


def test_deleted_row_with_its_count_lowered_fails(run: Run) -> None:
    """A deleted row still fails when the Counts table is lowered to match."""

    def delete(text: str) -> str:
        text = delete_line(text, "| `test_error_mapping` |")
        return replace_once(
            text,
            "| `tests/test_clients.py` | 21 | 21 | 0 | 0 |",
            "| `tests/test_clients.py` | 20 | 20 | 0 | 0 |",
        )

    status, out = run(delete)

    assert status == 1
    assert out == failed(
        [f"{MATRIX}: upstream tests/test_clients.py::test_error_mapping has no row"],
        summary(rows=128, rust=93),
    )


def test_wrong_count_fails(run: Run) -> None:
    """A Counts line that differs from the rows fails."""
    status, out = run(
        lambda text: replace_once(
            text,
            "| `tests/test_config.py` | 11 | 8 | 3 | 0 |",
            "| `tests/test_config.py` | 12 | 8 | 3 | 0 |",
        )
    )

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}: tests/test_config.py states 12 functions / 8 Rust / "
                "3 deviation / 0 excluded, the rows give 11 / 8 / 3 / 0"
            ),
        ],
        summary(),
    )


def test_counts_line_of_no_upstream_file_fails(run: Run) -> None:
    """A Counts line for a file upstream does not have fails."""
    clients = "| `tests/test_clients.py` | 21 | 21 | 0 | 0 |"

    status, out = run(
        lambda text: replace_once(
            text, clients, clients + "\n| `tests/test_phantom.py` | 0 | 0 | 0 | 0 |"
        )
    )

    assert status == 1
    assert out == failed(
        [f"{MATRIX}: tests/test_phantom.py is not an upstream test file"], summary()
    )


def test_second_row_for_one_test_fails(run: Run, repository: Path) -> None:
    """A row that repeats another row's upstream test fails."""
    line = line_of(pristine_matrix(repository), "| `test_error_mapping` |")

    status, out = run(
        lambda text: replace_once(
            text, "| `test_round_trip` |", "| `test_error_mapping` |"
        )
    )

    assert status == 1
    assert out == failed(
        [
            f"{MATRIX}:{line}: {CLIENTS}::test_error_mapping has a second row",
            f"{MATRIX}: upstream tests/test_clients.py::test_round_trip has no row",
        ],
        summary(),
    )


def test_reworded_deviation_fails(run: Run, repository: Path) -> None:
    """A deviation cell the README's table does not hold fails."""
    cell = "Timeout per httpx2 phase; `httpx2.Timeout` objects"
    line = line_of(pristine_matrix(repository), "| `test_timeout_object` |")

    status, out = run(
        edit_readme=lambda text: replace_once(
            text, f"| {cell} |", f"| Timeouts {cell[8:]} |"
        )
    )

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}:{line}: {CONFIG}::test_timeout_object: {cell!r} is not "
                f"the first cell of a row of {README}'s deviations table"
            )
        ],
        summary(),
    )


def fake_checkout(pin: Mapping[tuple[str, str], int | None], directory: Path) -> Path:
    """A checkout defining exactly the tests a pin names.

    Args:
        pin: The pin, ``(file, name) -> cases``.
        directory: Where the checkout is written.

    Returns:
        The checkout's root.
    """
    root = directory / "upstream"
    for file, name in sorted(pin):
        (root / file).parent.mkdir(parents=True, exist_ok=True)
        with (root / file).open("a", encoding="utf-8") as module:
            module.write(f"def {name}():\n    pass\n\n\n")
    return root


def fake_upstream(port_test_matrix: ModuleType, directory: Path) -> Path:
    """A checkout defining exactly the Python SDK's pinned upstream tests.

    Args:
        port_test_matrix: The loaded ``port-test-matrix.py``.
        directory: Where the checkout is written.

    Returns:
        The checkout's root.
    """
    return fake_checkout(port_test_matrix.UPSTREAM_TESTS, directory)


def test_upstream_matching_the_pin_passes(
    run: Run, port_test_matrix: ModuleType, tmp_path: Path
) -> None:
    """A checkout that defines exactly the pinned tests passes."""
    upstream = fake_upstream(port_test_matrix, tmp_path)

    status, out = run(argv=("--upstream", f"{SDK_NAME}={upstream}"))

    assert (status, out) == (0, summary() + "\n")


def test_upstream_function_missing_from_the_pin_fails(
    run: Run, port_test_matrix: ModuleType, tmp_path: Path
) -> None:
    """A test upstream defines and the pin lacks fails."""
    upstream = fake_upstream(port_test_matrix, tmp_path)
    with (upstream / "tests/test_config.py").open("a", encoding="utf-8") as module:
        module.write("async def test_new_thing():\n    pass\n")

    status, out = run(argv=("--upstream", f"{SDK_NAME}={upstream}"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{upstream}: {CONFIG}::test_new_thing is defined upstream but not "
                "in UPSTREAM_TESTS"
            )
        ],
        summary(),
    )


def test_pinned_function_missing_upstream_fails(
    run: Run, port_test_matrix: ModuleType, tmp_path: Path
) -> None:
    """A pinned test that upstream does not define fails."""
    upstream = fake_upstream(port_test_matrix, tmp_path)
    typing = upstream / "tests/test_typing.py"
    typing.write_text("def helper():\n    pass\n", encoding="utf-8")

    status, out = run(argv=("--upstream", f"{SDK_NAME}={upstream}"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{upstream}: tests/test_typing.py::test_public_typing is in "
                "UPSTREAM_TESTS but not defined upstream"
            )
        ],
        summary(),
    )


def test_upstream_file_missing_from_the_pin_fails(
    run: Run, port_test_matrix: ModuleType, tmp_path: Path
) -> None:
    """A test in a new upstream test file fails."""
    upstream = fake_upstream(port_test_matrix, tmp_path)
    (upstream / "tests/test_new.py").write_text(
        "def test_new():\n    pass\n", encoding="utf-8"
    )

    status, out = run(argv=("--upstream", f"{SDK_NAME}={upstream}"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{upstream}: tests/test_new.py::test_new is defined upstream but "
                "not in UPSTREAM_TESTS"
            ),
        ],
        summary(),
    )


def test_upstream_file_without_test_functions_fails(
    run: Run, port_test_matrix: ModuleType, tmp_path: Path
) -> None:
    """A new upstream test file that defines no test function at all fails."""
    upstream = fake_upstream(port_test_matrix, tmp_path)
    (upstream / "tests/test_helpers.py").write_text(
        "def helper():\n    pass\n", encoding="utf-8"
    )

    status, out = run(argv=("--upstream", f"{SDK_NAME}={upstream}"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{upstream}: tests/test_helpers.py defines no top-level test_* "
                "function; the pin cannot cover it"
            ),
        ],
        summary(),
    )


def test_upstream_file_of_test_methods_fails(
    run: Run, port_test_matrix: ModuleType, tmp_path: Path
) -> None:
    """A new upstream test file whose tests are methods of a class fails."""
    upstream = fake_upstream(port_test_matrix, tmp_path)
    (upstream / "tests/test_class.py").write_text(
        "class TestThing:\n    def test_method(self):\n        pass\n",
        encoding="utf-8",
    )

    status, out = run(argv=("--upstream", f"{SDK_NAME}={upstream}"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{upstream}: tests/test_class.py defines no top-level test_* "
                "function; the pin cannot cover it"
            ),
        ],
        summary(),
    )


def test_upstream_without_tests_fails(run: Run, tmp_path: Path) -> None:
    """A directory holding no upstream test file fails."""
    status, out = run(argv=("--upstream", f"{SDK_NAME}={tmp_path}"))

    assert status == 1
    assert out == failed([f"{tmp_path}: no tests/test_*.py found"], summary())


@pytest.mark.parametrize(
    ("values", "message"),
    [
        (("{checkout}",), "--upstream '{checkout}' is not NAME=CHECKOUT"),
        ((f"{SDK_NAME}=",), f"--upstream '{SDK_NAME}=' is not NAME=CHECKOUT"),
        (
            ("other={checkout}",),
            f"--upstream 'other={{checkout}}': 'other' is not one of {SDK_NAME}",
        ),
        (
            (f"{SDK_NAME}={{checkout}}", f"{SDK_NAME}={{checkout}}"),
            f"--upstream names '{SDK_NAME}' twice",
        ),
    ],
    ids=["a bare checkout", "no checkout", "an unknown name", "one name twice"],
)
def test_malformed_upstream_argument_fails(
    run: Run,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    values: tuple[str, ...],
    message: str,
) -> None:
    """``--upstream`` takes ``<name>=<checkout>`` with a known name, once each;
    anything else is a usage error, before any check runs."""
    checkout = str(tmp_path)
    argv = [
        item
        for value in values
        for item in ("--upstream", value.format(checkout=checkout))
    ]

    with pytest.raises(SystemExit) as exit_info:
        run(argv=tuple(argv))

    captured = capsys.readouterr()
    assert exit_info.value.code == 2
    assert captured.out == ""
    assert captured.err.endswith(f": error: {message.format(checkout=checkout)}\n")


# Every configuration: the Python SDK's, and a synthetic one with a
# ``tests/utils/`` file, an empty excludable set and the Counts layout that
# carries Cases. Each fault the checker refuses is planted in both.

Edit = Callable[[str], str]
#: The faults planted in every configuration, by the names the tests use.
MISSING_ROW = "missing row"
DANGLING_RUST_TEST = "dangling Rust test"
CASES_OFF_THE_PIN = "Cases off the pin"
COUNTS_SUM = "Counts sum"
EXCLUDED_WITH_CASES = "excluded row with Cases"
EXCLUDED_OUTSIDE_THE_SET = "excluded row outside the excludable files"

SYNTHETIC_NAME = "synthetic-upstream"
SYNTHETIC_MATRIX = "docs/synthetic-port-test-matrix.md"
SYNTHETIC_RATIO = "tests/utils/test_ratio.py"
SYNTHETIC_PIN: Mapping[tuple[str, str], int | None] = MappingProxyType(
    {
        ("tests/test_client.py", "test_round_trip"): 4,
        ("tests/test_client.py", "test_sync_client"): 2,
        (SYNTHETIC_RATIO, "test_ratio_sums_to_one"): 3,
    }
)
SYNTHETIC_RUST = """#[test]
fn ratio_sums_to_one() {}

#[tokio::test]
async fn round_trip_decodes() {}
"""
SYNTHETIC_README = """# Synthetic port

## Deviations from upstream

| Upstream | Here | Why |
| --- | --- | --- |
| No synchronous client | Async only | One client. |
"""
SYNTHETIC_PAGE = """# Synthetic matrix

## Counts

| Upstream file | Functions | Cases | Rust test | Deviation row |
| --- | ---: | ---: | ---: | ---: |
| `tests/test_client.py` | 2 | 6 | 1 | 1 |
| `tests/utils/test_ratio.py` | 1 | 3 | 1 | 0 |

## `tests/test_client.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_round_trip` | 4 | `src/lib_tests.rs::round_trip_decodes` |
| `test_sync_client` | 2 | Deviation: "No synchronous client" |

## `tests/utils/test_ratio.py`

| Upstream test | Cases | Covered by |
| --- | ---: | --- |
| `test_ratio_sums_to_one` | 3 | `src/lib_tests.rs::ratio_sums_to_one` |
"""
#: The summary line of the synthetic page, by its tallies.
synthetic_summary = partial(
    summary, rows=3, rust=2, deviation=1, excluded=0, tests=3, files=2
)


@dataclass(frozen=True)
class Planted:
    """A fault planted in one configuration's page, and what the checker prints.

    Attributes:
        edit: Turns the pristine page into the faulty one.
        faults: The fault lines, in the order they are printed.
        summary: The summary line for the faulty page.
    """

    edit: Edit
    faults: tuple[str, ...]
    summary: str


@dataclass(frozen=True)
class Scenario:
    """One upstream configuration, the only one the checker runs over.

    Attributes:
        upstream: Its ``Upstream``, an instance of a class of the loaded
            script, which no static type names.
        run: Writes the page an edit gives, runs the checker with the given
            arguments, and returns its status and standard output.
        summary: The summary line of the pristine page.
        planted: Each planted fault, by name.
    """

    upstream: Any
    run: Callable[..., tuple[int, str]]
    summary: str
    planted: Mapping[str, Planted]


def runner(
    port_test_matrix: ModuleType,
    upstream: Any,
    pristine: str,
    label: str,
    capsys: pytest.CaptureFixture[str],
) -> Callable[..., tuple[int, str]]:
    """A checker run over ``upstream`` alone, on edited copies of its page.

    Args:
        port_test_matrix: The loaded ``port-test-matrix.py``.
        upstream: The configuration; ``UPSTREAMS`` must hold only it.
        pristine: The page before any edit.
        label: The name the output gives the page's path.
        capsys: Captures the checker's output.

    Returns:
        A function of an edit and command-line arguments that returns the
        status and the standard output, the page named by ``label``.
    """

    def invoke(
        edit: Edit = lambda text: text, argv: tuple[str, ...] = ()
    ) -> tuple[int, str]:
        upstream.matrix.write_text(edit(pristine), encoding="utf-8")
        status = port_test_matrix.main(list(argv))
        return status, capsys.readouterr().out.replace(str(upstream.matrix), label)

    return invoke


@pytest.fixture
def sdk(
    port_test_matrix: ModuleType,
    repository: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> Scenario:
    """The Python SDK's configuration, on copies of the tracked page and README.

    It runs from the repository root, where the rows' Rust paths resolve.
    """
    readme = tmp_path / "README.md"
    readme.write_text((repository / README).read_text(encoding="utf-8"), "utf-8")
    upstream = replace(
        port_test_matrix.SDK, matrix=tmp_path / "port-test-matrix.md", readme=readme
    )
    monkeypatch.setattr(port_test_matrix, "UPSTREAMS", (upstream,))
    pristine = pristine_matrix(repository)

    row = "| `test_error_mapping` | 22 | `crates/sdk/tests/client.rs::error_mapping` |"
    where = f"{MATRIX}:{line_of(pristine, row)}: {CLIENTS}::test_error_mapping"
    counts = "| `tests/test_clients.py` | 21 | 21 | 0 | 0 |"
    markdown = "| `tests/test_docs.py` | `test_markdown` |"
    docs_section = (
        "## `tests/test_docs.py`\n\n"
        "| Upstream test | Cases | Covered by |\n"
        "| --- | ---: | --- |\n"
        "| `test_markdown` | 1 | Excluded: runs the Markdown examples |\n\n"
    )

    def docs_row_with_cases(text: str) -> str:
        text = delete_line(text, markdown)
        return replace_once(text, "## Excluded: ", docs_section + "## Excluded: ")

    def excluded_mapping(text: str) -> str:
        text = replace_once(
            text, row, "| `test_error_mapping` |  | Excluded: not wanted |"
        )
        return replace_once(
            text, counts, "| `tests/test_clients.py` | 21 | 20 | 0 | 1 |"
        )

    docs_line = line_of(docs_row_with_cases(pristine), "| `test_markdown` |")
    planted = {
        MISSING_ROW: Planted(
            lambda text: delete_line(text, row),
            (
                (
                    f"{MATRIX}: {CLIENTS} states 21 functions / 21 Rust / 0 deviation / "
                    "0 excluded, the rows give 20 / 20 / 0 / 0"
                ),
                f"{MATRIX}: upstream {CLIENTS}::test_error_mapping has no row",
            ),
            summary(rows=128, rust=93),
        ),
        DANGLING_RUST_TEST: Planted(
            lambda text: replace_once(
                text, row, row.replace("::error_mapping`", "::error_mappingz`")
            ),
            (
                (
                    f"{where}: crates/sdk/tests/client.rs has no test function "
                    "`error_mappingz`"
                ),
            ),
            summary(),
        ),
        CASES_OFF_THE_PIN: Planted(
            lambda text: replace_once(text, row, row.replace("| 22 |", "| 23 |")),
            (f"{where}: the case count is 23, but UPSTREAM_TESTS pins 22",),
            summary(),
        ),
        COUNTS_SUM: Planted(
            lambda text: replace_once(
                text, counts, "| `tests/test_clients.py` | 22 | 21 | 0 | 0 |"
            ),
            (
                (
                    f"{MATRIX}: {CLIENTS} states 22 functions / 21 Rust / 0 deviation / "
                    "0 excluded, the rows give 21 / 21 / 0 / 0"
                ),
            ),
            summary(),
        ),
        EXCLUDED_WITH_CASES: Planted(
            docs_row_with_cases,
            (
                (
                    f"{MATRIX}:{docs_line}: tests/test_docs.py::test_markdown: "
                    "excluded, but its Cases cell holds '1'; an excluded row has none"
                ),
            ),
            summary(),
        ),
        EXCLUDED_OUTSIDE_THE_SET: Planted(
            excluded_mapping,
            (
                (
                    f"{where}: excluded, but {CLIENTS} is not a tooling file "
                    f"({TOOLING_FILES})"
                ),
            ),
            summary(rust=93, excluded=19),
        ),
    }
    return Scenario(
        upstream,
        runner(port_test_matrix, upstream, pristine, MATRIX, capsys),
        summary(),
        planted,
    )


@pytest.fixture
def synthetic(
    port_test_matrix: ModuleType,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> Scenario:
    """A synthetic second configuration, written under the test's directory.

    Its pin has a ``tests/utils/`` file, its excludable set is empty, and its
    Counts table has the Cases column. It runs from its own root, where the
    rows' Rust paths resolve.
    """
    root = tmp_path / "synthetic"
    (root / "src").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / "src/lib_tests.rs").write_text(SYNTHETIC_RUST, encoding="utf-8")
    (root / "README.md").write_text(SYNTHETIC_README, encoding="utf-8")
    monkeypatch.chdir(root)
    upstream = port_test_matrix.Upstream(
        name=SYNTHETIC_NAME,
        matrix=Path(SYNTHETIC_MATRIX),
        readme=Path("README.md"),
        deviations_heading="## Deviations from upstream",
        deviations_header="Upstream",
        pin=SYNTHETIC_PIN,
        pin_name="SYNTHETIC_TESTS",
        excludable=frozenset(),
        counts_columns=("functions", "cases", "rust", "deviation"),
    )
    monkeypatch.setattr(port_test_matrix, "UPSTREAMS", (upstream,))

    row = "| `test_ratio_sums_to_one` | 3 | `src/lib_tests.rs::ratio_sums_to_one` |"
    line = line_of(SYNTHETIC_PAGE, row)
    where = f"{SYNTHETIC_MATRIX}:{line}: {SYNTHETIC_RATIO}::test_ratio_sums_to_one"
    counts = "| `tests/utils/test_ratio.py` | 1 | 3 | 1 | 0 |"

    def edit_row_and_counts(new_row: str, new_counts: str) -> Edit:
        def edit(text: str) -> str:
            text = replace_once(text, row, new_row)
            return replace_once(text, counts, new_counts)

        return edit

    excluded_not_allowed = (
        f"{where}: excluded, but no file of {SYNTHETIC_NAME} may be excluded"
    )
    planted = {
        MISSING_ROW: Planted(
            lambda text: delete_line(text, row),
            (
                (
                    f"{SYNTHETIC_MATRIX}: {SYNTHETIC_RATIO} states 1 functions / "
                    "3 cases / 1 Rust / 0 deviation, the rows give 0 / 0 / 0 / 0"
                ),
                (
                    f"{SYNTHETIC_MATRIX}: upstream {SYNTHETIC_RATIO}::"
                    "test_ratio_sums_to_one has no row"
                ),
            ),
            synthetic_summary(rows=2, rust=1),
        ),
        DANGLING_RUST_TEST: Planted(
            lambda text: replace_once(
                text, "::ratio_sums_to_one`", "::ratio_sums_to_onez`"
            ),
            (f"{where}: src/lib_tests.rs has no test function `ratio_sums_to_onez`",),
            synthetic_summary(),
        ),
        # The Counts line follows the row, so only the pin can tell.
        CASES_OFF_THE_PIN: Planted(
            edit_row_and_counts(
                row.replace("| 3 |", "| 4 |"),
                "| `tests/utils/test_ratio.py` | 1 | 4 | 1 | 0 |",
            ),
            (f"{where}: the case count is 4, but SYNTHETIC_TESTS pins 3",),
            synthetic_summary(),
        ),
        COUNTS_SUM: Planted(
            lambda text: replace_once(
                text, counts, "| `tests/utils/test_ratio.py` | 1 | 4 | 1 | 0 |"
            ),
            (
                (
                    f"{SYNTHETIC_MATRIX}: {SYNTHETIC_RATIO} states 1 functions / "
                    "4 cases / 1 Rust / 0 deviation, the rows give 1 / 3 / 1 / 0"
                ),
            ),
            synthetic_summary(),
        ),
        EXCLUDED_WITH_CASES: Planted(
            edit_row_and_counts(
                "| `test_ratio_sums_to_one` | 3 | Excluded: not wanted |",
                "| `tests/utils/test_ratio.py` | 1 | 3 | 0 | 0 |",
            ),
            (
                excluded_not_allowed,
                (
                    f"{where}: excluded, but its Cases cell holds '3'; an excluded "
                    "row has none"
                ),
            ),
            synthetic_summary(rust=1, excluded=1),
        ),
        EXCLUDED_OUTSIDE_THE_SET: Planted(
            edit_row_and_counts(
                "| `test_ratio_sums_to_one` |  | Excluded: not wanted |",
                "| `tests/utils/test_ratio.py` | 1 | 0 | 0 | 0 |",
            ),
            (excluded_not_allowed,),
            synthetic_summary(rust=1, excluded=1),
        ),
    }
    return Scenario(
        upstream,
        runner(port_test_matrix, upstream, SYNTHETIC_PAGE, SYNTHETIC_MATRIX, capsys),
        synthetic_summary(),
        planted,
    )


@pytest.fixture(params=["sdk", "synthetic"])
def configuration(request: pytest.FixtureRequest) -> Scenario:
    """Each configuration in turn: the Python SDK's, then the synthetic one."""
    return request.getfixturevalue(request.param)


def assert_planted_fails(configuration: Scenario, fault: str) -> None:
    """Plant one fault and compare the whole output with the expected one.

    Args:
        configuration: The configuration the fault is planted in.
        fault: The fault's name.
    """
    planted = configuration.planted[fault]

    status, out = configuration.run(planted.edit)

    assert status == 1
    assert out == failed(list(planted.faults), planted.summary)


def test_every_configuration_passes_untouched(configuration: Scenario) -> None:
    """The pristine page of each configuration passes."""
    status, out = configuration.run()

    assert (status, out) == (0, configuration.summary + "\n")


def test_every_configuration_refuses_a_missing_row(configuration: Scenario) -> None:
    """An upstream function without a row fails."""
    assert_planted_fails(configuration, MISSING_ROW)


def test_every_configuration_refuses_a_dangling_rust_test(
    configuration: Scenario,
) -> None:
    """A row naming a Rust function that is not a test fails."""
    assert_planted_fails(configuration, DANGLING_RUST_TEST)


def test_every_configuration_refuses_cases_off_the_pin(
    configuration: Scenario,
) -> None:
    """A row whose Cases differs from the pinned number fails."""
    assert_planted_fails(configuration, CASES_OFF_THE_PIN)


def test_every_configuration_refuses_a_counts_sum_that_differs(
    configuration: Scenario,
) -> None:
    """A Counts line that differs from the rows fails."""
    assert_planted_fails(configuration, COUNTS_SUM)


def test_every_configuration_refuses_an_excluded_row_with_cases(
    configuration: Scenario,
) -> None:
    """An excluded row with a Cases number fails."""
    assert_planted_fails(configuration, EXCLUDED_WITH_CASES)


def test_every_configuration_refuses_an_excluded_row_outside_its_files(
    configuration: Scenario,
) -> None:
    """An excluded row of a file outside the excludable set fails; with an
    empty set, any excluded row does."""
    assert_planted_fails(configuration, EXCLUDED_OUTSIDE_THE_SET)


def test_mapped_row_pinned_as_excluded_fails(sdk: Scenario, repository: Path) -> None:
    """A row that maps a test the pin holds as excluded fails on its Cases."""
    markdown = "| `tests/test_docs.py` | `test_markdown` |"
    section = (
        "## `tests/test_docs.py`\n\n"
        "| Upstream test | Cases | Covered by |\n"
        "| --- | ---: | --- |\n"
        "| `test_markdown` | 1 | `crates/sdk/tests/client.rs::error_mapping` |\n\n"
    )

    def edit(text: str) -> str:
        text = delete_line(text, markdown)
        text = replace_once(text, "## Excluded: ", section + "## Excluded: ")
        return replace_once(
            text,
            "| `tests/test_docs.py` | 2 | 0 | 0 | 2 |",
            "| `tests/test_docs.py` | 2 | 1 | 0 | 1 |",
        )

    line = line_of(edit(pristine_matrix(repository)), "| `test_markdown` |")

    status, out = sdk.run(edit)

    assert status == 1
    assert out == failed(
        [
            (
                f"{MATRIX}:{line}: tests/test_docs.py::test_markdown: the case count "
                "is 1, but UPSTREAM_TESTS pins this test as excluded"
            )
        ],
        summary(rust=95, excluded=17),
    )


def test_deviations_header_is_not_a_deviation(synthetic: Scenario) -> None:
    """A row quoting the deviations table's header cell fails."""
    line = line_of(SYNTHETIC_PAGE, "| `test_sync_client` |")

    status, out = synthetic.run(
        lambda text: replace_once(
            text, 'Deviation: "No synchronous client"', 'Deviation: "Upstream"'
        )
    )

    assert status == 1
    assert out == failed(
        [
            (
                f"{SYNTHETIC_MATRIX}:{line}: tests/test_client.py::test_sync_client: "
                "'Upstream' is not the first cell of a row of README.md's deviations "
                "table"
            )
        ],
        synthetic_summary(),
    )


def test_upstream_utils_files_are_read(synthetic: Scenario, tmp_path: Path) -> None:
    """``--upstream`` reads ``tests/utils/test_*.py`` as well as ``tests/test_*.py``."""
    checkout = fake_checkout(SYNTHETIC_PIN, tmp_path)
    argv = ("--upstream", f"{SYNTHETIC_NAME}={checkout}")

    assert synthetic.run(argv=argv) == (0, synthetic.summary + "\n")

    with (checkout / SYNTHETIC_RATIO).open("a", encoding="utf-8") as module:
        module.write("def test_ratio_of_nothing():\n    pass\n")
    (checkout / "tests/utils/test_helpers.py").write_text(
        "def helper():\n    pass\n", encoding="utf-8"
    )

    status, out = synthetic.run(argv=argv)

    assert status == 1
    assert out == failed(
        [
            (
                f"{checkout}: tests/utils/test_helpers.py defines no top-level "
                "test_* function; the pin cannot cover it"
            ),
            (
                f"{checkout}: {SYNTHETIC_RATIO}::test_ratio_of_nothing is defined "
                "upstream but not in SYNTHETIC_TESTS"
            ),
        ],
        synthetic.summary,
    )


def test_two_upstreams_are_checked_and_named(
    port_test_matrix: ModuleType,
    synthetic: Scenario,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With two upstreams, each summary names its upstream, a fault of one
    leaves the other passing, and ``--upstream`` checks only the named one."""
    second_matrix = Path("docs/second-port-test-matrix.md")
    second_matrix.write_text(
        delete_line(SYNTHETIC_PAGE, "| `test_round_trip` |"), encoding="utf-8"
    )
    second = replace(synthetic.upstream, name="second-upstream", matrix=second_matrix)
    monkeypatch.setattr(port_test_matrix, "UPSTREAMS", (synthetic.upstream, second))
    checkout = fake_checkout(SYNTHETIC_PIN, tmp_path)
    (checkout / "tests/test_client.py").write_text(
        "def test_sync_client():\n    pass\n", encoding="utf-8"
    )

    status, out = synthetic.run(argv=("--upstream", f"second-upstream={checkout}"))

    assert status == 1
    assert out == failed(
        [
            (
                f"{second_matrix}: tests/test_client.py states 2 functions / 6 cases "
                "/ 1 Rust / 1 deviation, the rows give 1 / 2 / 0 / 1"
            ),
            f"{second_matrix}: upstream tests/test_client.py::test_round_trip has no row",
            (
                f"{checkout}: tests/test_client.py::test_round_trip is in "
                "SYNTHETIC_TESTS but not defined upstream"
            ),
        ],
        f"{SYNTHETIC_NAME}: {synthetic.summary}\n"
        f"second-upstream: {synthetic_summary(rows=2, rust=1)}",
    )


# The Python adapter's configuration, on copies of its tracked page and README.

ADAPTER_MATRIX = "docs/adapter-port-test-matrix.md"
ADAPTER_README = "crates/adapter/README.md"
CONFIDENCE = "tests/utils/test_confidence_metrics.py"
EXCLUDED_ROW = "excluded row"
METRICS_TESTS = "crates/adapter/src/metrics_tests.rs"
SCHEMA_TESTS = "crates/adapter/src/schema_tests.rs"
CONFIDENCE_LINE = f"// Upstream: {CONFIDENCE}::test_confidence_metrics"
LABELS_LINE = "// Upstream: tests/test_schema.py::test_probability_labels_preserve_arbitrary_names"
#: The summary line of the adapter's page, by its tallies.
adapter_summary = partial(
    summary, rows=71, rust=63, deviation=8, excluded=0, tests=71, files=12
)


@pytest.fixture
def adapter(
    port_test_matrix: ModuleType,
    repository: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> Scenario:
    """The Python adapter's configuration, on copies of the tracked page and README.

    It runs from the repository root, where the rows' Rust paths resolve.
    """
    readme = tmp_path / "README.md"
    readme.write_text(
        (repository / ADAPTER_README).read_text(encoding="utf-8"), "utf-8"
    )
    upstream = replace(
        port_test_matrix.ADAPTER,
        matrix=tmp_path / "adapter-port-test-matrix.md",
        readme=readme,
    )
    monkeypatch.setattr(port_test_matrix, "UPSTREAMS", (upstream,))
    pristine = (repository / ADAPTER_MATRIX).read_text(encoding="utf-8")

    row = (
        "| `test_confidence_metrics` | 8 | "
        "`crates/adapter/src/metrics_tests.rs::metrics_upstream_confidence` |"
    )
    where = (
        f"{ADAPTER_MATRIX}:{line_of(pristine, row)}: "
        f"{CONFIDENCE}::test_confidence_metrics"
    )
    counts = f"| `{CONFIDENCE}` | 1 | 8 | 1 | 0 |"
    # One target of a row that names two Rust tests.
    schema_target = (
        "`crates/adapter/src/schema_tests.rs::"
        "probability_labels_preserve_arbitrary_names`"
    )
    labels = "| `test_probability_labels_preserve_arbitrary_names` |"
    labels_where = (
        f"{ADAPTER_MATRIX}:{line_of(pristine, labels)}: tests/test_schema.py::"
        "test_probability_labels_preserve_arbitrary_names"
    )
    # The `// Upstream:` lines of the two rows' tests, which a fault in a row
    # leaves without a row that lists their test.
    metrics = (repository / METRICS_TESTS).read_text(encoding="utf-8")
    confidence_line = (
        f"{METRICS_TESTS}:{line_of(metrics, CONFIDENCE_LINE)}: {CONFIDENCE_LINE}"
    )
    schema = (repository / SCHEMA_TESTS).read_text(encoding="utf-8")
    labels_line = f"{SCHEMA_TESTS}:{line_of(schema, LABELS_LINE)}: {LABELS_LINE}"

    def edit_row_and_counts(new_row: str, new_counts: str) -> Edit:
        def edit(text: str) -> str:
            text = replace_once(text, row, new_row)
            return replace_once(text, counts, new_counts)

        return edit

    planted = {
        MISSING_ROW: Planted(
            lambda text: delete_line(text, row),
            (
                (
                    f"{ADAPTER_MATRIX}: {CONFIDENCE} states 1 functions / 8 cases / "
                    "1 Rust / 0 deviation, the rows give 0 / 0 / 0 / 0"
                ),
                (
                    f"{ADAPTER_MATRIX}: upstream {CONFIDENCE}::"
                    "test_confidence_metrics has no row"
                ),
                f"{confidence_line} names no row of {ADAPTER_MATRIX}",
            ),
            adapter_summary(rows=70, rust=62),
        ),
        DANGLING_RUST_TEST: Planted(
            lambda text: replace_once(
                text, schema_target, schema_target.replace("_names`", "_namez`")
            ),
            (
                (
                    f"{labels_where}: crates/adapter/src/schema_tests.rs has no test "
                    "function `probability_labels_preserve_arbitrary_namez`"
                ),
                (
                    f"{labels_line} is above "
                    "`probability_labels_preserve_arbitrary_names`, which the row at "
                    f"{ADAPTER_MATRIX}:{line_of(pristine, labels)} does not list"
                ),
            ),
            adapter_summary(),
        ),
        # The Counts line follows the row, so only the pin can tell.
        CASES_OFF_THE_PIN: Planted(
            edit_row_and_counts(
                row.replace("| 8 |", "| 9 |"), f"| `{CONFIDENCE}` | 1 | 9 | 1 | 0 |"
            ),
            (f"{where}: the case count is 9, but ADAPTER_TESTS pins 8",),
            adapter_summary(),
        ),
        # The Counts line follows the row, so only the empty excludable set
        # can tell.
        EXCLUDED_ROW: Planted(
            edit_row_and_counts(
                "| `test_confidence_metrics` |  | Excluded: not wanted |",
                f"| `{CONFIDENCE}` | 1 | 0 | 0 | 0 |",
            ),
            (
                (
                    f"{where}: excluded, but no file of system-one-adapter-python "
                    "may be excluded"
                ),
                (
                    f"{confidence_line} is above `metrics_upstream_confidence`, which "
                    f"the row at {ADAPTER_MATRIX}:{line_of(pristine, row)} does not list"
                ),
            ),
            adapter_summary(rust=62, excluded=1),
        ),
        COUNTS_SUM: Planted(
            lambda text: replace_once(
                text, counts, f"| `{CONFIDENCE}` | 1 | 9 | 1 | 0 |"
            ),
            (
                (
                    f"{ADAPTER_MATRIX}: {CONFIDENCE} states 1 functions / 9 cases / "
                    "1 Rust / 0 deviation, the rows give 1 / 8 / 1 / 0"
                ),
            ),
            adapter_summary(),
        ),
    }
    return Scenario(
        upstream,
        runner(port_test_matrix, upstream, pristine, ADAPTER_MATRIX, capsys),
        adapter_summary(),
        planted,
    )


def test_tracked_pages_pass_together(
    port_test_matrix: ModuleType,
    repository: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Both tracked pages pass in the form CI runs, each summary named."""
    status = port_test_matrix.main([])

    assert (status, capsys.readouterr().out) == (
        0,
        f"{SDK_NAME}: {summary()}\nsystem-one-adapter-python: {adapter_summary()}\n",
    )


def test_adapter_page_passes_untouched(adapter: Scenario) -> None:
    """The adapter's tracked page passes on its own."""
    status, out = adapter.run()

    assert (status, out) == (0, adapter.summary + "\n")


def test_adapter_matrix_refuses_a_deleted_row(adapter: Scenario) -> None:
    """An upstream function of the adapter without a row fails."""
    assert_planted_fails(adapter, MISSING_ROW)


def test_adapter_matrix_refuses_a_renamed_rust_test(adapter: Scenario) -> None:
    """One renamed Rust test among a row's targets fails."""
    assert_planted_fails(adapter, DANGLING_RUST_TEST)


def test_adapter_matrix_refuses_cases_off_the_pin(adapter: Scenario) -> None:
    """A row whose Cases differs from the collected number fails."""
    assert_planted_fails(adapter, CASES_OFF_THE_PIN)


def test_adapter_matrix_refuses_an_excluded_row(adapter: Scenario) -> None:
    """No function of the adapter's upstream may be excluded."""
    assert_planted_fails(adapter, EXCLUDED_ROW)


def test_adapter_matrix_refuses_a_counts_sum_that_differs(adapter: Scenario) -> None:
    """A Counts line whose Cases differs from the rows' sum fails."""
    assert_planted_fails(adapter, COUNTS_SUM)


# The adapter's `// Upstream:` lines, on a copy of the Rust sources the checker
# reads, so that a line can be planted, changed or removed.

NORMALIZATION = "tests/utils/test_probability_normalization.py"


@dataclass(frozen=True)
class Tree:
    """A copy of the adapter's page, README and Rust sources to run the checker on.

    Attributes:
        root: The copy's root, the checker's working directory.
        run: Runs the checker over the adapter alone and returns its status
            and standard output.
        page: The tracked page's text.
    """

    root: Path
    run: Callable[[], tuple[int, str]]
    page: str

    def edit(self, path: str, change: Edit) -> str:
        """Change one copied file, and return its new text.

        Args:
            path: The file, relative to the copy's root.
            change: Turns the file's text into the new one.

        Returns:
            The new text.
        """
        file = self.root / path
        text = change(file.read_text(encoding="utf-8"))
        file.write_text(text, encoding="utf-8")
        return text


@pytest.fixture
def adapter_tree(
    port_test_matrix: ModuleType,
    repository: Path,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> Tree:
    """The adapter's configuration on a copy of every file the checker reads.

    The copy holds the page, the README, every Rust source under the
    annotated directories and every file a row names, at their tracked paths;
    the checker runs from the copy's root.
    """
    adapter = port_test_matrix.ADAPTER
    page = (repository / ADAPTER_MATRIX).read_text(encoding="utf-8")
    matrix = port_test_matrix.read_matrix(page, adapter.counts_columns)
    sources = {
        file.relative_to(repository)
        for place in adapter.annotated
        for file in (repository / place).rglob("*.rs")
    }
    sources.update(
        Path(path)
        for row in matrix.rows
        for path, _ in port_test_matrix.RUST_TARGET.findall(row.target)
    )
    root = tmp_path / "tree"
    for relative in sorted({*sources, Path(ADAPTER_MATRIX), Path(ADAPTER_README)}):
        (root / relative).parent.mkdir(parents=True, exist_ok=True)
        (root / relative).write_bytes((repository / relative).read_bytes())
    monkeypatch.chdir(root)
    monkeypatch.setattr(port_test_matrix, "UPSTREAMS", (adapter,))

    def run() -> tuple[int, str]:
        status = port_test_matrix.main([])
        return status, capsys.readouterr().out

    return Tree(root, run, page)


def fault_row(tree: Tree, row: str) -> str:
    """The ``<page>:<line>: <file>::<name>`` that starts a fault of a row.

    Args:
        tree: The copy.
        row: The upstream function as ``<file>::<name>``.

    Returns:
        The page and the row's line, then the function.
    """
    name = row.split("::")[1]
    return f"{ADAPTER_MATRIX}:{line_of(tree.page, f'| `{name}` |')}: {row}"


def test_adapter_tree_passes_untouched(adapter_tree: Tree) -> None:
    """The copy of the tracked files passes, every line held to its row."""
    assert adapter_tree.run() == (0, adapter_summary() + "\n")


def test_adapter_matrix_refuses_a_removed_upstream_line(adapter_tree: Tree) -> None:
    """A test a row names that lost its ``// Upstream:`` line fails."""
    text = adapter_tree.edit(
        METRICS_TESTS, lambda text: delete_line(text, CONFIDENCE_LINE)
    )
    function = line_of(text, "fn metrics_upstream_confidence()")

    assert adapter_tree.run() == (
        1,
        failed(
            [
                (
                    f"{fault_row(adapter_tree, f'{CONFIDENCE}::test_confidence_metrics')}: "
                    f"`metrics_upstream_confidence` at {METRICS_TESTS}:{function} carries "
                    f"no `{CONFIDENCE_LINE}` line"
                )
            ],
            adapter_summary(),
        ),
    )


def test_adapter_matrix_refuses_an_upstream_line_naming_another_function(
    adapter_tree: Tree,
) -> None:
    """A line that names a function whose row lists another test fails, and
    the test it was taken from has lost its own line."""
    other = (
        f"// Upstream: {NORMALIZATION}::test_probability_normalization_and_debug_data"
    )
    text = adapter_tree.edit(
        METRICS_TESTS, lambda text: replace_once(text, CONFIDENCE_LINE, other)
    )
    # The tracked file holds the other function's line too, above its own test.
    line = next(
        number
        for number, content in enumerate(text.split("\n"), start=1)
        if content == other
    )
    function = line_of(text, "fn metrics_upstream_confidence()")
    normalization_row = line_of(
        adapter_tree.page, "| `test_probability_normalization_and_debug_data` |"
    )

    assert adapter_tree.run() == (
        1,
        failed(
            [
                (
                    f"{METRICS_TESTS}:{line}: {other} is above "
                    "`metrics_upstream_confidence`, which the row at "
                    f"{ADAPTER_MATRIX}:{normalization_row} does not list"
                ),
                (
                    f"{fault_row(adapter_tree, f'{CONFIDENCE}::test_confidence_metrics')}"
                    f": `metrics_upstream_confidence` at {METRICS_TESTS}:{function} "
                    f"carries no `{CONFIDENCE_LINE}` line"
                ),
            ],
            adapter_summary(),
        ),
    )


def test_adapter_matrix_refuses_an_upstream_line_above_a_test_no_row_names(
    adapter_tree: Tree,
) -> None:
    """A line above a test that its row does not list fails."""
    text = adapter_tree.edit(
        METRICS_TESTS,
        lambda text: replace_once(
            text,
            "#[test]\nfn metrics_tie_mode() {",
            f"#[test]\n{CONFIDENCE_LINE}\nfn metrics_tie_mode() {{",
        ),
    )
    line = line_of(text, "fn metrics_tie_mode()") - 1
    confidence_row = line_of(adapter_tree.page, "| `test_confidence_metrics` |")

    assert adapter_tree.run() == (
        1,
        failed(
            [
                (
                    f"{METRICS_TESTS}:{line}: {CONFIDENCE_LINE} is above "
                    f"`metrics_tie_mode`, which the row at {ADAPTER_MATRIX}:"
                    f"{confidence_row} does not list"
                )
            ],
            adapter_summary(),
        ),
    )


def test_adapter_matrix_refuses_an_upstream_line_naming_no_row(
    adapter_tree: Tree,
) -> None:
    """A line naming a function that has no row fails."""
    made_up = f"// Upstream: {CONFIDENCE}::test_made_up"
    text = adapter_tree.edit(
        METRICS_TESTS,
        lambda text: replace_once(
            text,
            "#[test]\nfn metrics_tie_mode() {",
            f"#[test]\n{made_up}\nfn metrics_tie_mode() {{",
        ),
    )
    line = line_of(text, made_up)

    assert adapter_tree.run() == (
        1,
        failed(
            [f"{METRICS_TESTS}:{line}: {made_up} names no row of {ADAPTER_MATRIX}"],
            adapter_summary(),
        ),
    )


@pytest.mark.parametrize(
    ("planted", "fault"),
    [
        (
            "// Upstream: test_confidence_metrics",
            (
                "'// Upstream: test_confidence_metrics' is not "
                "`// Upstream: <file>::<function>`"
            ),
        ),
        (
            f"\n// Upstream: {CONFIDENCE}::test_elsewhere",
            f"// Upstream: {CONFIDENCE}::test_elsewhere is not directly above a test",
        ),
    ],
    ids=["malformed", "separated by a blank line"],
)
def test_adapter_matrix_refuses_an_upstream_line_out_of_place(
    adapter_tree: Tree, planted: str, fault: str
) -> None:
    """A line not of the one form, or not in a test's block, fails."""
    text = adapter_tree.edit(
        METRICS_TESTS,
        lambda text: replace_once(
            text,
            "#[test]\nfn metrics_tie_mode() {",
            f"{planted}\n\n#[test]\nfn metrics_tie_mode() {{",
        ),
    )
    line = line_of(text, planted.strip())

    assert adapter_tree.run() == (
        1,
        failed([f"{METRICS_TESTS}:{line}: {fault}"], adapter_summary()),
    )
