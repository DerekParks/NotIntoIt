"""Score filled IRS forms in a workspace against a bench answer key.
usage: python3 bench/grade.py <answers.json> <workspace>/<year>/forms [--nii path]"""

import json
import math
import re
import subprocess
import sys
from pathlib import Path


def fields(nii: str, pdf: Path) -> list[dict]:
    out = subprocess.run([nii, "form", "fields", str(pdf)], check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def amount(raw):
    if raw is None or str(raw).strip() in ("", "-", "-0-"):
        return 0.0
    s = str(raw).strip().replace("$", "").replace(",", "").replace(" ", "")
    negative = s.startswith("(") and s.endswith(")") or s.startswith("-")
    s = s.strip("()-")
    try:
        value = -float(s) if negative else float(s)
        return value if math.isfinite(value) else None
    except ValueError:
        return None


def checked(value) -> bool:
    return value not in (None, "", "Off")


def find(form_fields: list[dict], key: str) -> list[dict]:
    """Description substring, or '=description' for exact matching; 'pN|' limits page."""
    page = None
    if m := re.match(r"p(\d+)\|", key):
        page, key = int(m.group(1)), key[m.end():]
    exact = key.startswith("=")
    if exact:
        key = key[1:]
    norm = lambda d: re.sub(r"\s+", " ", d or "")
    return [f for f in form_fields if (key == norm(f.get("description")) if exact
                                     else key in norm(f.get("description")))
            and page in (None, f.get("page"))]


def matches(want, got, key: str, tol: float) -> bool:
    """Whether a field's value satisfies an answer: a string, number, checkbox state,
    {"contains": text}, or {"any_of": [answers]} where "" stands for a blank field. A number never
    matches a blank field."""
    if isinstance(want, dict) and "contains_any" in want:
        return any(text.lower() in str(got or "").lower() for text in want["contains_any"])
    if isinstance(want, dict) and "any_of" in want:
        return any(matches(w, got, key, tol) for w in want["any_of"])
    if isinstance(want, dict):
        return want["contains"].lower() in (got or "").lower()
    if isinstance(want, bool):
        return checked(got) == want
    if isinstance(want, (int, float)):
        g = amount(got)
        return bool(str(got or "").strip()) and g is not None and abs(g - want) <= tol
    g, w = (re.sub(r"[\s-]", "", x or "").lower() for x in (got, want))
    first_name = re.search(r"first name|name\. first", key, re.I)
    return g == w or bool(first_name and w and re.fullmatch(re.escape(w) + r"[a-z]?\.?", g))


def row_layout(form_fields: list[dict], spec: dict, tol: float):
    """Match an entire populated table against declared, unordered alternatives.

    Every active row is consumed once. Correct totals cannot hide duplicate owners,
    extra rows, missing identifiers, or offsetting incorrect amounts.
    """
    numeric_columns = {column for layout in spec['alternatives'] for row in layout
                       for column, value in row.items()
                       if isinstance(value, (int, float)) and not isinstance(value, bool)}
    actual = []
    for selectors in spec['rows']:
        row = {}
        for column, selector in selectors.items():
            hits = find(form_fields, selector)
            if len(hits) != 1:
                raise ValueError(f'row column {selector!r} matched {len(hits)} fields')
            row[column] = hits[0].get('value')
        if not any(str(v or '').strip() for v in row.values()):
            continue
        # An otherwise empty row with a numeric zero is an unused table row.
        if all(not str(value or '').strip() or (column in numeric_columns and amount(value) == 0)
               for column, value in row.items()):
            continue
        actual.append(row)

    def assign(expected, remaining):
        if not expected:
            return not remaining
        pattern, *rest = expected
        return any(
            pattern.keys() == row.keys()
            and all(matches(want, row[column], column, tol) for column, want in pattern.items())
            and assign(rest, remaining[:i] + remaining[i + 1:])
            for i, row in enumerate(remaining)
        )

    return actual, any(len(pattern) == len(actual) and assign(pattern, actual)
                       for pattern in spec['alternatives'])


def grade(answers: dict, forms_dir: Path, nii: str) -> dict:
    tol = answers.get("tolerance_dollars", 1)
    results, problems = [], []
    missing, blank, unreadable = [], [], []
    cache, layouts = {}, {}
    def read(stem):
        if stem not in cache:
            pdf = Path(forms_dir) / f"{stem}.pdf"
            try:
                cache[stem] = fields(nii, pdf) if pdf.exists() else []
            except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
                unreadable.append(f"{stem}: {e}")
                cache[stem] = []
        return cache[stem]
    for stem, spec in answers["forms"].items():
        pdf = Path(forms_dir) / f"{stem}.pdf"
        ff = read(stem)
        if not pdf.exists() and spec.get("required", True):
            missing.append(stem)
        if pdf.exists() and not any(f.get("value") for f in ff) and spec.get("required", True):
            blank.append(stem)
        if not any(f.get("value") for f in ff):
            for key in spec["fields"]:
                results.append((stem, key, spec["fields"][key], "BLANK FORM" if pdf.exists() else "MISSING FORM", False))
            continue
        for key, want in spec["fields"].items():
            if isinstance(want, dict) and 'row_layout' in want:
                layout_name = want['row_layout']
                layout_key = (stem, layout_name)
                if layout_key not in layouts:
                    try:
                        layouts[layout_key] = row_layout(ff, spec['row_layouts'][layout_name], tol)
                    except (KeyError, ValueError) as e:
                        problems.append(f'{stem}: invalid row layout {layout_name!r}: {e}')
                        layouts[layout_key] = (str(e), False)
                got, ok = layouts[layout_key]
                results.append((stem, key, want, got, ok))
                continue
            hits = find(ff, key)
            if len(hits) != 1:
                problems.append(f"{stem}: answer key {key!r} matched {len(hits)} fields")
                results.append((stem, key, want, f"<{len(hits)} matches>", False))
                continue
            got = hits[0].get("value")
            results.append((stem, key, want, got, matches(want, got, key, tol)))
    # Downloading a blank reference form does not mean it was included in the return.
    extra = [s for s in answers.get("not_required", []) if any(f.get("value") for f in read(s))]
    consistency = []
    status_keys = ["Filing Status. Check only one box. Single.", "Married filing jointly",
                   "Married filing separately", "Head of household", "Qualifying surviving spouse"]
    status_fields = [f for f in read("f1040")
                     if any((f.get("description") or "").startswith(key) for key in status_keys)]
    if status_fields and sum(checked(f.get("value")) for f in status_fields) != 1:
        consistency.append("Form 1040 must select exactly one filing status")
    for check in answers.get("reconciliations", []):
        def value(ref):
            hits = find(read(ref["form"]), ref["field"])
            return amount(hits[0].get("value")) if len(hits) == 1 else None
        total = value(check["total"])
        parts = [value(ref) for ref in check["parts"]]
        if total is None or any(p is None for p in parts) or abs(total - sum(parts)) > tol:
            consistency.append(check["name"])
    passed = sum(r[4] for r in results)
    bottom_line = outcome(answers, Path(forms_dir), nii, tol, read)
    return {
        "passed": passed,
        "total": len(results),
        **bottom_line,
        "return_complete": not (missing or blank or unreadable or problems or consistency or extra)
                           and passed == len(results) and bottom_line["outcome_correct"],
        "missing_required_forms": missing,
        "blank_required_forms": blank,
        "unreadable_forms": unreadable,
        "consistency_problems": consistency,
        "results": results,
        "unneeded_forms": extra,
        "key_problems": problems,
    }


def outcome(answers: dict, forms_dir: Path, nii: str, tol: float, read=None) -> dict:
    """The return's bottom line: refund minus amount owed, against the expected value."""
    spec = answers.get("outcome")
    pdf = forms_dir / f"{spec['form']}.pdf" if spec else None
    if not spec or not pdf.exists():
        return {"net_refund": None, "outcome_error": None, "outcome_correct": False}
    ff = read(spec["form"]) if read else fields(nii, pdf)
    value = lambda key: (lambda hits: amount(hits[0].get("value")) if len(hits) == 1 else None)(find(ff, key))
    refund, owe = value(spec["refund"]), value(spec["owe"])
    populated = any(str(f.get("value") or "").strip() for key in (spec["refund"], spec["owe"])
                    for f in find(ff, key))
    if not populated or refund is None or owe is None or refund < 0 or owe < 0 or (refund > 0 and owe > 0):
        return {"net_refund": None, "outcome_error": None, "outcome_correct": False}
    net = refund - owe
    error = abs(net - spec["expected"])
    return {"net_refund": net, "outcome_error": round(error, 2), "outcome_correct": error <= tol}


def main():
    args = sys.argv[1:]
    nii = "target/release/nii"
    if "--nii" in args:
        i = args.index("--nii"); nii = args[i + 1]; del args[i:i + 2]
    answers = json.load(open(args[0]))
    report = grade(answers, Path(args[1]), nii)
    for stem, key, want, got, ok in report["results"]:
        if not ok:
            print(f"FAIL {stem:8} {key[:70]!r}: want {want!r}, got {got!r}")
    for p in report["key_problems"]:
        print("KEY", p)
    for p in report["consistency_problems"] + report["unreadable_forms"]:
        print("RETURN", p)
    if report["unneeded_forms"]:
        print("unneeded forms filed:", ", ".join(report["unneeded_forms"]))
    print(f"score {report['passed']}/{report['total']}; refund {report['net_refund']} "
          f"({'correct' if report['outcome_correct'] else f'off by {report["outcome_error"]}'})")
    print(f"complete graded return: {report['return_complete']}")


if __name__ == "__main__":
    main()
