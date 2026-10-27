"""Checks of the kernel side, run with any Python that has IPython, pandas
and matplotlib: ``python crates/stet-app/python/tests/test_stet.py``."""

import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from IPython.core.interactiveshell import InteractiveShell  # noqa: E402

shell = InteractiveShell.instance()
import matplotlib  # noqa: E402

matplotlib.use("agg")
import stet  # noqa: E402

root = tempfile.mkdtemp()
stet._attach(theme={"text": "#222222", "muted": "#777777", "grid": "#eeeeee", "series": ["#1e66f5", "#fe640b"]},
             refs={}, root=root)
stet._before_run(None)
import matplotlib.pyplot as plt  # noqa: E402
import pandas as pd  # noqa: E402

form = shell.display_formatter.format


def test_frames_are_tables():
    frame = pd.DataFrame({"week": pd.date_range("2024-01-01", periods=2, freq="D"), "n": [3, 1]})
    data, _ = form(frame)
    assert data[stet.TABLE] == {"columns": ["week", "n"], "rows": [["2024-01-01", 3], ["2024-01-02", 1]], "total": 2}
    data, _ = form(frame.set_index("week")["n"])
    assert data[stet.TABLE]["columns"] == ["week", "n"]


def test_simple_figures_are_charts_and_others_are_not():
    fig, ax = plt.subplots()
    ax.plot([1, 2, 3], [2, 4, 3], label="a")
    ax.plot([1, 2, 3], [1, 1, 2], label="b")
    ax.set_title("t")
    spec = form(fig)[0][stet.CHART]
    assert spec["mark"] == "line" and spec["title"] == "t"
    assert {row["series"] for row in spec["data"]["values"]} == {"a", "b"}
    fig, ax = plt.subplots()
    ax.bar(["x", "y"], [3, 5])
    spec = form(fig)[0][stet.CHART]
    assert spec["mark"] == "bar" and spec["data"]["values"][1]["x"] == "y"
    fig, ax = plt.subplots()
    ax.imshow([[1, 2], [3, 4]])
    assert stet.CHART not in form(fig)[0]
    plt.close("all")


def test_figure_formats_do_not_lose_charts():
    shell.run_line_magic("config", "InlineBackend.figure_formats = ['png']")
    stet._figures_back()
    fig, ax = plt.subplots()
    ax.plot([1, 2], [1, 2])
    assert stet.CHART in form(fig)[0]
    plt.close("all")


def test_refs_load_lazily_and_publish_saves():
    path = os.path.join(root, "a.csv")
    with open(path, "w") as out:
        out.write("x,y\n1,2\n")
    stet._set_refs({"a": path})
    assert list(stet.refs.a.columns) == ["x", "y"]
    assert stet.refs["a"] is stet.refs.a, "cached until the file changes"
    try:
        stet.refs.missing
    except KeyError as error:
        assert "a" in str(error)
    else:
        raise AssertionError("an unknown ref is an error")
    relative = stet.publish(pd.DataFrame({"z": [1]}), "clean")
    assert relative.startswith(".stet/data/clean.")
    assert list(stet.refs.clean.columns) == ["z"]


if __name__ == "__main__":
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
        print("ok", test.__name__)
