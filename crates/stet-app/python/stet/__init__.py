"""Stet's side of a notebook kernel.

Stet loads this into every Python kernel it starts; there is nothing to
install. It makes outputs look at home in the editor:

- pandas DataFrames and Series are sent as tables Stet draws itself;
- matplotlib figures follow the editor's theme, and simple ones (lines,
  bars, scatter, on one set of axes) are sent as native charts, with values
  under the pointer; others come as SVG;
- Altair charts are sent as Vega-Lite, which Stet draws natively;
- Plotly figures made of scatter and bar traces are sent as native charts.

`stet.refs.<name>` loads the data a document names in a link reference
definition (``[signups]: data/signups.csv``), and `stet.publish(df, name)`
saves a frame and offers the document a ref to it.
"""

import json
import math
import os
import sys

TABLE = "application/vnd.stet.table+json"
CHART = "application/vnd.stet.chart+json"
PUBLISH = "application/vnd.stet.publish+json"

#: Most rows of a table sent to the editor.
TABLE_ROWS = 200

_theme = {}
_root = None
_refs_paths = {}
_refs_cache = {}
_attached = False


# --- Attaching ---------------------------------------------------------------


def _attach(theme=None, refs=None, root=None):
    """Called by Stet once a kernel starts (and after a restart)."""
    global _attached, _root
    if root:
        _root = root
    if refs is not None:
        _set_refs(refs)
    if theme:
        _set_theme(theme)
    if _attached:
        return
    _attached = True
    shell = _shell()
    if shell is None:
        return
    # By the module a class says it is from, which moves between versions
    # (pandas 3 says `pandas`, earlier ones `pandas.core.frame`).
    _formatter(shell, TABLE, [
        ("pandas", "DataFrame", _frame_table),
        ("pandas.core.frame", "DataFrame", _frame_table),
        ("pandas", "Series", _series_table),
        ("pandas.core.series", "Series", _series_table),
        ("polars", "DataFrame", _polars_table),
        ("polars.dataframe.frame", "DataFrame", _polars_table),
    ])
    _formatter(shell, CHART, [
        ("matplotlib.figure", "Figure", _figure_chart),
        ("plotly.graph_objs", "Figure", _plotly_chart),
        ("plotly.graph_objs._figure", "Figure", _plotly_chart),
    ])
    shell.user_ns.setdefault("refs", refs_object)
    # matplotlib's inline backend takes figures from every formatter when
    # it starts and resets its look: start it now, ahead of the writer's
    # code, and put figures back before each cell's figures are shown.
    if "matplotlib" not in sys.modules and _installed("matplotlib"):
        try:
            import matplotlib.pyplot  # noqa: F401
        except Exception:
            pass
    shell.events.register("pre_run_cell", _before_run)
    shell.events.register("post_execute", _figures_back)
    _before_run(None)


def _installed(name):
    import importlib.util
    try:
        return importlib.util.find_spec(name) is not None
    except Exception:
        return False


def _shell():
    try:
        from IPython import get_ipython
    except ImportError:
        return None
    return get_ipython()


def _formatter(shell, mime, types):
    """A display formatter for `mime`, for the types named (module, class)."""
    from IPython.core.formatters import BaseFormatter

    formatters = shell.display_formatter.formatters
    formatter = formatters.get(mime)
    if formatter is None:
        class StetFormatter(BaseFormatter):
            format_type = mime
            print_method = "_repr_stet_"
            _return_type = dict

        formatter = StetFormatter(parent=shell.display_formatter)
        formatters[mime] = formatter
    active = shell.display_formatter.active_types
    if mime not in active:
        active.append(mime)
    for module, name, function in types:
        formatter.for_type_by_name(module, name, _quietly(function))


def _quietly(function):
    """A formatter that cannot break an output: a failure means "not this
    form", and the next form shows."""
    def formatter(value):
        try:
            return function(value)
        except Exception:
            return None
    return formatter


def _before_run(_info):
    """Before each cell: libraries imported since get Stet's settings."""
    if "matplotlib" in sys.modules and not getattr(_before_run, "matplotlib", False):
        _before_run.matplotlib = True
        _matplotlib_setup()
    _figures_back()
    if "altair" in sys.modules and not getattr(_before_run, "altair", False):
        _before_run.altair = True
        try:
            sys.modules["altair"].renderers.enable("mimetype")
        except Exception:
            pass


def _figures_back():
    """Choosing figure formats (`%config InlineBackend.figure_formats`)
    takes figures from every formatter, ours too: put it back."""
    figure = sys.modules.get("matplotlib.figure")
    shell = _shell()
    chart = shell.display_formatter.formatters.get(CHART) if shell else None
    if figure is not None and chart is not None:
        chart.for_type(figure.Figure, _quietly(_figure_chart))


# --- Theme ---------------------------------------------------------------------


def _set_theme(theme):
    _theme.clear()
    _theme.update(theme)
    if "matplotlib" in sys.modules:
        _matplotlib_theme()


def _matplotlib_setup():
    shell = _shell()
    if shell is not None:
        try:
            # A drawing stays sharp at any size; figures the walker cannot
            # read come as SVG.
            from matplotlib_inline.backend_inline import set_matplotlib_formats
            set_matplotlib_formats("svg")
        except Exception:
            pass
    _matplotlib_theme()


def _matplotlib_theme():
    if not _theme:
        return
    try:
        import matplotlib as mpl
        from cycler import cycler
    except ImportError:
        return
    text, muted, grid = _theme.get("text"), _theme.get("muted"), _theme.get("grid")
    rc = {
        "figure.facecolor": "none",
        "axes.facecolor": "none",
        "savefig.facecolor": "none",
        "savefig.transparent": True,
        "axes.edgecolor": muted,
        "axes.labelcolor": muted,
        "axes.titlecolor": text,
        "axes.grid": True,
        "axes.grid.axis": "y",
        "grid.color": grid,
        "axes.spines.top": False,
        "axes.spines.right": False,
        "xtick.color": muted,
        "ytick.color": muted,
        "text.color": text,
        "legend.frameon": False,
        "font.family": "sans-serif",
        "font.sans-serif": ["Helvetica Neue", "Helvetica", "Arial", "DejaVu Sans"],
    }
    if _theme.get("series"):
        rc["axes.prop_cycle"] = cycler(color=_theme["series"])
    mpl.rcParams.update({key: value for key, value in rc.items() if value is not None})


# --- Tables --------------------------------------------------------------------


def _plain(value):
    """A cell as JSON can carry it."""
    if value is None:
        return None
    if isinstance(value, float):
        return None if math.isnan(value) or math.isinf(value) else value
    if isinstance(value, (bool, int, str)):
        return value
    item = getattr(value, "item", None)
    if callable(item):
        try:
            return _plain(item())
        except Exception:
            pass
    isoformat = getattr(value, "isoformat", None)
    if callable(isoformat):
        text = isoformat()
        # A timestamp at midnight is a date.
        return text[:-9] if text.endswith("T00:00:00") else text
    return str(value)


def table(columns, rows, total=None):
    """A table in Stet's shape, from column names and rows."""
    rows = [[_plain(cell) for cell in row] for row in rows]
    return {"columns": [str(column) for column in columns], "rows": rows,
            "total": len(rows) if total is None else total}


def _frame_table(frame):
    head = frame.head(TABLE_ROWS)
    columns = [str(column) for column in head.columns]
    rows = head.itertuples(index=False, name=None)
    index = head.index
    named = any(name is not None for name in getattr(index, "names", [None]))
    if named or not _plain_index(index):
        names = [str(name) if name is not None else "" for name in index.names]
        columns = names + columns
        rows = ((*(key if isinstance(key, tuple) else (key,)), *row)
                for key, row in zip(index, head.itertuples(index=False, name=None)))
    return table(columns, rows, len(frame))


def _plain_index(index):
    """0, 1, 2, ...: an index not worth a column."""
    try:
        import pandas as pd
        return isinstance(index, pd.RangeIndex) and index.start == 0 and index.step == 1
    except ImportError:
        return False


def _series_table(series):
    return _frame_table(series.to_frame(name=series.name if series.name is not None else "value"))


def _polars_table(frame):
    head = frame.head(TABLE_ROWS)
    return table(head.columns, head.iter_rows(), frame.height)


# --- Charts: matplotlib --------------------------------------------------------


def _hex(color):
    from matplotlib.colors import to_hex
    return to_hex(color)


def _figure_chart(figure):
    """A figure with one set of axes holding lines, bars or scatter, as a
    native chart; anything else is left to SVG."""
    axes = [ax for ax in figure.get_axes() if ax.get_visible()]
    if len(axes) != 1:
        return None
    ax = axes[0]
    if ax.images or ax.patches and not ax.containers:
        return None
    if not all(_is_scatter(collection) for collection in ax.collections):
        return None
    if ax.get_xscale() != "linear" or ax.get_yscale() != "linear":
        return None
    dates = _dates(ax.xaxis)
    categories = _tick_categories(ax)
    # (mark, series name, rows), one per thing drawn.
    parts = []

    def name_of(label):
        return label if label and not label.startswith("_") else "series %d" % (len(parts) + 1)

    for line in ax.get_lines():
        if not line.get_visible():
            continue
        rows = [{"x": _x_value(x, dates, categories), "y": _plain(y)}
                for x, y in zip(line.get_xdata(), line.get_ydata())]
        dotted = line.get_linestyle() in ("None", "", " ") and line.get_marker() not in (None, "None", "", " ")
        parts.append(("point" if dotted else "line", name_of(line.get_label()), rows))
    for container in ax.containers:
        patches = getattr(container, "patches", None)
        if not patches:
            continue
        if any(patch.get_y() != 0 for patch in patches) or any(patch.get_height() < 0 for patch in patches):
            return None  # stacked by hand, or across: SVG draws it as drawn
        rows = [{"x": _x_value(patch.get_x() + patch.get_width() / 2, dates, categories),
                 "y": _plain(patch.get_height())} for patch in patches]
        parts.append(("bar", name_of(container.get_label()), rows))
    for collection in ax.collections:
        rows = [{"x": _x_value(x, dates, categories), "y": _plain(y)} for x, y in collection.get_offsets()]
        parts.append(("point", name_of(collection.get_label()), rows))
    if not parts:
        return None
    bars = any(mark == "bar" for mark, _, _ in parts)
    if bars and any(mark != "bar" for mark, _, _ in parts):
        return None
    x_type = "temporal" if dates else ("nominal" if categories or bars else "quantitative")
    encoding = {"x": {"field": "x", "type": x_type}, "y": {"field": "y", "type": "quantitative"}}
    # An axis matplotlib leaves unlabelled stays so.
    encoding["x"]["title"] = ax.get_xlabel()
    encoding["y"]["title"] = ax.get_ylabel()
    if len(parts) > 1:
        encoding["color"] = {"field": "series", "type": "nominal", "title": ""}
    layers = {}
    for mark, name, rows in parts:
        layers.setdefault(mark, []).extend(dict(row, series=name) for row in rows)
    spec = {"encoding": encoding}
    if len(layers) == 1:
        (mark, values), = layers.items()
        spec.update(mark=mark, data={"values": values})
    else:
        spec["layer"] = [{"mark": mark, "data": {"values": values}} for mark, values in layers.items()]
    if ax.get_title():
        spec["title"] = ax.get_title()
    _, height = figure.get_size_inches()
    spec["height"] = round(min(360.0, max(160.0, height * 60)))
    return spec


def _is_scatter(collection):
    from matplotlib.collections import PathCollection
    return isinstance(collection, PathCollection)


def _dates(axis):
    try:
        from matplotlib.dates import DateConverter, _SwitchableDateConverter
        return isinstance(axis.get_converter(), (DateConverter, _SwitchableDateConverter))
    except Exception:
        try:
            from matplotlib import dates
            return isinstance(axis.converter, (dates.DateConverter,))
        except Exception:
            return False


def _x_value(x, dates, categories):
    if dates:
        from matplotlib.dates import num2date
        try:
            text = num2date(float(x)).replace(tzinfo=None).isoformat()
            return text[:-9] if text.endswith("T00:00:00") else text
        except Exception:
            return _plain(x)
    if categories:
        label = categories.get(round(float(x), 6))
        if label is not None:
            return label
    return _plain(x)


def _tick_categories(ax):
    """Category names, when the x axis holds strings (``plt.bar(['a', 'b'], ..)``)."""
    units = getattr(ax.xaxis, "units", None)
    mapping = getattr(units, "_mapping", None)
    if not mapping:
        return {}
    return {round(float(at), 6): str(name) for name, at in mapping.items()}


# --- Charts: Plotly ------------------------------------------------------------


def _plotly_chart(figure):
    """Scatter and bar traces as a native chart."""
    data = figure.to_plotly_json()
    traces = data.get("data", [])
    if not traces:
        return None
    values, marks = [], []
    for at, trace in enumerate(traces):
        kind = trace.get("type", "scatter")
        if kind not in ("scatter", "scattergl", "bar"):
            return None
        name = trace.get("name") or "trace %d" % (at + 1)
        xs, ys = trace.get("x"), trace.get("y")
        if ys is None:
            return None
        if xs is None:
            xs = range(len(ys))
        if trace.get("orientation") == "h":
            return None
        for x, y in zip(list(xs), list(ys)):
            values.append({"x": _plain(x), "y": _plain(y), "series": name})
        mode = trace.get("mode") or "lines"
        marks.append("bar" if kind == "bar" else ("line" if "lines" in mode else "point"))
    x_type = "nominal" if any(isinstance(v["x"], str) and not _looks_like_date(v["x"]) for v in values) else (
        "temporal" if any(isinstance(v["x"], str) for v in values) else "quantitative")
    if "bar" in marks and x_type == "quantitative":
        x_type = "ordinal"
    layout = data.get("layout", {})
    title = layout.get("title")
    title = title.get("text") if isinstance(title, dict) else title
    axis_title = lambda axis: ((layout.get(axis) or {}).get("title") or {}).get("text")
    encoding = {"x": {"field": "x", "type": x_type}, "y": {"field": "y", "type": "quantitative"}}
    if axis_title("xaxis"):
        encoding["x"]["title"] = axis_title("xaxis")
    if axis_title("yaxis"):
        encoding["y"]["title"] = axis_title("yaxis")
    if len(traces) > 1:
        encoding["color"] = {"field": "series", "type": "nominal"}
    spec = {"data": {"values": values}, "encoding": encoding}
    if len(set(marks)) == 1:
        spec["mark"] = marks[0]
    else:
        spec["layer"] = [
            {"mark": mark, "data": {"values": [v for v, m in zip(values, _expand(marks, traces)) if m == mark]}}
            for mark in dict.fromkeys(marks)
        ]
        spec.pop("data")
    if title:
        spec["title"] = title
    return spec


def _expand(marks, traces):
    """Each value's mark, in the order values were gathered."""
    for mark, trace in zip(marks, traces):
        for _ in trace.get("y") or []:
            yield mark


def _looks_like_date(text):
    return len(text) >= 10 and text[4] == "-" and text[7] == "-" and text[:4].isdigit()


# --- Data refs -----------------------------------------------------------------


def _set_refs(paths):
    _refs_paths.clear()
    _refs_paths.update(paths)


class Refs:
    """The data a document names, by name: ``refs.signups`` loads
    ``[signups]: data/signups.csv`` the first time it is asked for, and
    again only when the file changes."""

    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        return self[name]

    def __getitem__(self, name):
        path = _refs_paths.get(name)
        if path is None:
            known = ", ".join(sorted(_refs_paths)) or "none"
            raise KeyError("the document names no data called %r (it names: %s)" % (name, known))
        stamp = os.path.getmtime(path)
        cached = _refs_cache.get(name)
        if cached and cached[0] == (path, stamp):
            return cached[1]
        value = load(path)
        _refs_cache[name] = ((path, stamp), value)
        return value

    def __dir__(self):
        return sorted(_refs_paths)

    def __iter__(self):
        return iter(sorted(_refs_paths))

    def __repr__(self):
        return "refs(%s)" % ", ".join(sorted(_refs_paths))


refs_object = Refs()
refs = refs_object


def load(path):
    """A data file as a DataFrame (a SQLite file as a connection)."""
    lower = path.lower()
    if lower.endswith((".sqlite", ".sqlite3", ".db")):
        import sqlite3
        return sqlite3.connect(path)
    import pandas as pd
    if lower.endswith(".csv"):
        return pd.read_csv(path)
    if lower.endswith(".tsv"):
        return pd.read_csv(path, sep="\t")
    if lower.endswith(".parquet"):
        return pd.read_parquet(path)
    if lower.endswith((".arrow", ".feather")):
        return pd.read_feather(path)
    if lower.endswith((".jsonl", ".ndjson")):
        return pd.read_json(path, lines=True)
    if lower.endswith(".json"):
        return pd.read_json(path)
    raise ValueError("stet cannot read %s" % path)


def publish(frame, name):
    """Save `frame` beside the document as ``.stet/data/<name>.parquet`` (or
    ``.csv`` without pyarrow), and offer the document a ref to it: Stet
    shows it as a suggestion to accept."""
    import re
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", name):
        raise ValueError("a ref's name is letters, digits, - and _: %r" % name)
    root = _root or os.getcwd()
    folder = os.path.join(root, ".stet", "data")
    os.makedirs(folder, exist_ok=True)
    try:
        path = os.path.join(folder, name + ".parquet")
        frame.to_parquet(path)
    except Exception:
        path = os.path.join(folder, name + ".csv")
        frame.to_csv(path, index=False)
    relative = os.path.relpath(path, root).replace(os.sep, "/")
    _refs_paths[name] = path
    _refs_cache.pop(name, None)
    from IPython.display import display
    rows = len(frame) if hasattr(frame, "__len__") else None
    display({PUBLISH: {"name": name, "path": relative, "rows": rows},
             "text/plain": "published %s → %s" % (name, relative)}, raw=True)
    return relative


__all__ = ["refs", "load", "publish", "table", "TABLE", "CHART"]
