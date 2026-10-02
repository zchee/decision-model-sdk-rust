"""Behaviour of ``port-test-matrix.py`` on the tracked page and on planted errors.

Every error is planted on a copy of the page or of the README under the test's
temporary directory; the tracked files are only read.
"""

from collections.abc import Callable, Mapping
from dataclasses import replace
from pathlib import Path
from types import ModuleType

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
