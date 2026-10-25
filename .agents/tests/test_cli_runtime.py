"""Expose CLI runtime tests to the shared plugin unittest discovery."""

import importlib.util
from pathlib import Path
import unittest


def load_tests(loader, tests, pattern):
    path = Path(__file__).resolve().parents[2] / "tests" / "test_cli_operations.py"
    specification = importlib.util.spec_from_file_location("cli_runtime_operations", path)
    module = importlib.util.module_from_spec(specification)
    assert specification.loader is not None
    specification.loader.exec_module(module)
    tests.addTests(unittest.TestLoader().loadTestsFromModule(module))
    return tests
