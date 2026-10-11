"""Score filled IRS forms in a workspace against a bench answer key.
usage: python3 bench/grade.py <answers.json> <workspace>/<year>/forms [--nii path]"""

import json
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
        return -float(s) if negative else float(s)
    except ValueError:
        return None


def checked(value) -> bool:
    return value not in (None, "", "Off")


def find(form_fields: list[dict], key: str) -> list[dict]:
    """Fields whose description contains `key`; a "pN|" prefix restricts to page N."""
    page = None
    if m := re.match(r"p(\d+)\|", key):
        page, key = int(m.group(1)), key[m.end():]
    norm = lambda d: re.sub(r"\s+", " ", d or "")
    return [f for f in form_fields if key in norm(f.get("description")) and page in (None, f.get("page"))]


def matches(want, got, key: str, tol: float) -> bool:
    """Whether a field's value satisfies an answer: a string, number, checkbox state,
    {"contains": text}, or {"any_of": [answers]} where "" stands for a blank field. A number never
    matches a blank field."""
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


def grade(answers: dict, forms_dir: Path, nii: str) -> dict:
    tol = answers.get("tolerance_dollars", 1)
    results, problems = [], []
    for stem, spec in answers["forms"].items():
        pdf = Path(forms_dir) / f"{stem}.pdf"
        ff = fields(nii, pdf) if pdf.exists() else []
        if not any(f.get("value") for f in ff):
            for key in spec["fields"]:
                results.append((stem, key, spec["fields"][key], "BLANK FORM" if pdf.exists() else "MISSING FORM", False))
            continue
        for key, want in spec["fields"].items():
            hits = find(ff, key)
            if len(hits) != 1:
                problems.append(f"{stem}: answer key {key!r} matched {len(hits)} fields")
                results.append((stem, key, want, f"<{len(hits)} matches>", False))
                continue
            got = hits[0].get("value")
            results.append((stem, key, want, got, matches(want, got, key, tol)))
    extra = [s for s in answers.get("not_required", []) if (Path(forms_dir) / f"{s}.pdf").exists()]
    passed = sum(r[4] for r in results)
    return {
        "passed": passed,
        "total": len(results),
        **outcome(answers, Path(forms_dir), nii, tol),
        "results": results,
        "unneeded_forms": extra,
        "key_problems": problems,
    }


def outcome(answers: dict, forms_dir: Path, nii: str, tol: float) -> dict:
    """The return's bottom line: refund minus amount owed, against the expected value."""
    spec = answers.get("outcome")
    pdf = forms_dir / f"{spec['form']}.pdf" if spec else None
    if not spec or not pdf.exists():
        return {"net_refund": None, "outcome_error": None, "outcome_correct": False}
    ff = fields(nii, pdf)
    value = lambda key: (lambda hits: amount(hits[0].get("value")) if len(hits) == 1 else None)(find(ff, key))
    refund, owe = value(spec["refund"]), value(spec["owe"])
    if refund is None or owe is None:
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
    if report["unneeded_forms"]:
        print("unneeded forms filed:", ", ".join(report["unneeded_forms"]))
    print(f"score {report['passed']}/{report['total']}; refund {report['net_refund']} "
          f"({'correct' if report['outcome_correct'] else f'off by {report["outcome_error"]}'})")


if __name__ == "__main__":
    main()
