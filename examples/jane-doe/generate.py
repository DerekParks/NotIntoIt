"""Generate Jane Doe's 2025 source documents into 2025/documents/ (see README.md).
Every person, company, and number is fictional; SSNs use area 000, which is never issued."""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from pypdf import PdfReader, PdfWriter
from reportlab.lib import colors
from reportlab.lib.pagesizes import letter
from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet
from reportlab.lib.units import inch
from reportlab.platypus import PageBreak, Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle

HERE = Path(__file__).resolve().parent
DOCS = HERE / "2025" / "documents"
REPO = HERE.parents[1]
TAX_YEAR = 2025

JANE = {
    "first": "Jane A", "last": "Doe", "ssn": "000-12-6789", "ssn_masked": "XXX-XX-6789",
    "street": "1428 Elm Street, Apt 2B", "city_state_zip": "Denver, CO 80203",
}

W2 = {
    "a. Employee's social security number": JANE["ssn"],
    "b. Employer identification number": "00-1234567",
    "c. Employer's name, address": "Bramblewood Analytics, Inc.\n500 Market Street, Suite 300\nDenver, CO 80202",
    "d. Control number": "A1-0042",
    "e. Employee's first name": JANE["first"],
    "e. Last name": JANE["last"],
    "f. Employee's address": f"{JANE['street']}\n{JANE['city_state_zip']}",
    "1. Wages, tips": "72805.00",
    "2. Federal income tax withheld": "6980.00",
    "3. Social security wages": "78860.00",
    "4. Social security tax withheld": "4889.32",
    "5. Medicare wages": "78860.00",
    "6. Medicare tax withheld": "1143.47",
    "10. Dependent care benefits": "5000.00",
    "12a. Code": "D",
    "12a. Amount": "6055.00",
    "12b. Code": "DD",
    "12b. Amount": "8940.00",
    "13. Retirement plan": True,
    "14. Other": "CO FAMLI 389.25",
    "15. State. 1": "CO",
    "15. Employer's state I D number. 1": "00000123",
    "16. State wages, tips, etc. 1": "72805.00",
    "17. State income tax. 1": "2874.00",
}

BANK_1099_INT = {
    "For calendar year": str(TAX_YEAR),
    "PAYER’S name": "Prairie Federal Savings Bank\n210 Sixteenth Street\nDenver, CO 80202\n(303) 555-0142",
    "PAYER’S T I N": "00-7654321",
    "RECIPIENT’S T I N": JANE["ssn_masked"],
    "RECIPIENT’S name": f"{JANE['first']} {JANE['last']}",
    "Street address": JANE["street"],
    "City or town": JANE["city_state_zip"],
    "Account number": "****4417 (High-Yield Savings)",
    "1. Interest income": "643.17",
}

STUDENT_LOAN_1098_E = {
    "RECIPIENT’S/LENDER’S name": "Granite Student Loan Servicing\nPO Box 4410\nSalt Lake City, UT 84110",
    "RECIPIENT’S T I N": "00-2468135",
    "BORROWER’S T I N": JANE["ssn_masked"],
    "BORROWER’S name": f"{JANE['first']} {JANE['last']}",
    "Street address": JANE["street"],
    "City or town": JANE["city_state_zip"],
    "Account number": "9001-55-2231",
    "1. Student loan interest received by lender": "1184.52",
}

# (description, symbol, qty, acquired, sold, proceeds, cost, wash_disallowed, term)
SALES = [
    ("INITECH SOFTWARE CORP", "INTK", 15, "10/12/2022", "03/21/2025", 1779.00, 2100.00, 0.00, "long"),
    ("NORTHWIND TOTAL MARKET ETF", "NWTM", 40, "02/11/2021", "06/18/2025", 10856.00, 7930.00, 0.00, "long"),
    ("ACME ROBOTICS INC", "ACMR", 50, "02/03/2025", "08/04/2025", 4065.00, 5105.00, 1040.00, "short"),
    ("GLOBEX CORP", "GBX", 25, "04/07/2025", "09/29/2025", 1946.25, 1530.00, 0.00, "short"),
    ("ACME ROBOTICS INC", "ACMR", 50, "02/21/2025", "11/14/2025", 4637.50, 5190.00, 0.00, "short"),
]
# (description, ordinary, qualified, capital gain distributions, foreign tax)
DIVIDENDS = [
    ("NORTHWIND TOTAL MARKET ETF (NWTM)", 351.40, 340.00, 0.00, 0.00),
    ("HARBORVIEW INTERNATIONAL EQUITY ETF (HVIE)", 134.80, 72.75, 63.40, 14.62),
]
BROKERAGE_INTEREST = 21.84
DAYCARE_MONTHLY = 1100.00


def main() -> None:
    nii = find_nii()
    DOCS.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, NII_WORKSPACE=tmp)
        irs_form(nii, env, "fw2", W2, ["CopyB", "CopyC", "Copy2"], DOCS / "w2-bramblewood-analytics.pdf")
        irs_form(nii, env, "f1099int", BANK_1099_INT, ["CopyB"], DOCS / "1099-int-prairie-federal-savings.pdf")
        irs_form(nii, env, "f1098e", STUDENT_LOAN_1098_E, ["CopyB"], DOCS / "1098-e-granite-student-loans.pdf")
    brokerage_statement(DOCS / "loyalty-brokerage-2025-consolidated-1099.pdf")
    daycare_statement(DOCS / "little-sprouts-2025-childcare-statement.pdf")
    for path in sorted(DOCS.iterdir()):
        print(path.relative_to(REPO))


def find_nii() -> str:
    for candidate in (os.environ.get("NII_BIN"), REPO / "target" / "release" / "nii", shutil.which("nii")):
        if candidate and Path(candidate).exists():
            return str(candidate)
    sys.exit("nii not found: build it (cargo build --release -p nii) or set NII_BIN")


def run(nii: str, env: dict, *args: str) -> str:
    return subprocess.run([nii, *args], env=env, check=True, capture_output=True, text=True).stdout.strip()


def irs_form(nii: str, env: dict, stem: str, data: dict, copies: list[str], out: Path) -> None:
    """Fill `data` (description prefix -> value) into each copy, flatten, keep only those copies' pages."""
    blank = run(nii, env, "form", "fetch", stem, "--year", str(TAX_YEAR), "--overwrite")
    fields = json.loads(run(nii, env, "form", "fields", blank))
    values, pages = {}, set()
    for copy in copies:
        in_copy = [f for f in fields if f".{copy}[0]." in f["name"]]
        for prefix, value in data.items():
            hits = [f for f in in_copy if normalized(f.get("description", "")).startswith(prefix)]
            if len(hits) != 1:
                sys.exit(f"{stem} {copy}: {prefix!r} matched {[h['name'] for h in hits]}")
            values[hits[0]["name"]] = value
            pages.add(hits[0]["page"])
    work = Path(env["NII_WORKSPACE"])
    (work / "values.json").write_text(json.dumps(values))
    filled, flat = work / f"{stem}-filled.pdf", work / f"{stem}-flat.pdf"
    run(nii, env, "form", "fill", blank, "--values", str(work / "values.json"), "--output", str(filled))
    run(nii, env, "form", "flatten", str(filled), "--output", str(flat))
    reader, writer = PdfReader(flat), PdfWriter()
    for page in sorted(pages):
        writer.add_page(reader.pages[page - 1])
    with out.open("wb") as f:
        writer.write(f)


def normalized(description: str) -> str:
    return re.sub(r"^Page \d+\. Copy \w+\. ", "", description).replace("See instructions for box 12. ", "")


STYLES = getSampleStyleSheet()
SMALL = ParagraphStyle("small", parent=STYLES["Normal"], fontSize=8, leading=10)
CELL = ParagraphStyle("cell", parent=SMALL, leading=9)
HEAD = ParagraphStyle("head", parent=CELL, fontName="Helvetica-Bold")
GRID = TableStyle([
    ("FONT", (0, 0), (-1, -1), "Helvetica", 8),
    ("FONT", (0, 0), (-1, 0), "Helvetica-Bold", 8),
    ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#E8EEF4")),
    ("GRID", (0, 0), (-1, -1), 0.4, colors.HexColor("#9AA8B6")),
    ("ALIGN", (1, 1), (-1, -1), "RIGHT"),
    ("VALIGN", (0, 0), (-1, -1), "MIDDLE"),
])
LEFT_DESCRIPTION = TableStyle(GRID.getCommands() + [("ALIGN", (1, 1), (1, -1), "LEFT")])


def cells(row: list, style: ParagraphStyle = CELL, wrap: int = 1) -> list:
    """Wrap the first `wrap` cells as paragraphs so long text stays inside its column."""
    return [Paragraph(str(c), style) if i < wrap else c for i, c in enumerate(row)]


def money(x: float) -> str:
    return f"({abs(x):,.2f})" if x < 0 else f"{x:,.2f}"


def brokerage_statement(out: Path) -> None:
    doc = SimpleDocTemplate(str(out), pagesize=letter, leftMargin=0.6 * inch, rightMargin=0.6 * inch,
                            topMargin=0.6 * inch, bottomMargin=0.6 * inch,
                            title="Loyalty Brokerage 2025 Tax Reporting Statement")
    s = [
        Paragraph("<b>LOYALTY BROKERAGE SERVICES LLC</b>", STYLES["Title"]),
        Paragraph("2025 TAX REPORTING STATEMENT — Consolidated Form 1099", STYLES["Heading2"]),
        Table([
            ["PAYER", "RECIPIENT"],
            ["Loyalty Brokerage Services LLC\n100 Summit Avenue\nBoston, MA 02110\nFederal ID 00-1357913",
             f"{JANE['first']} {JANE['last']}\n{JANE['street']}\n{JANE['city_state_zip']}\nTIN {JANE['ssn_masked']}"],
            ["Account 4471-2231 (Individual)", "Date prepared 02/14/2026"],
        ], colWidths=[3.6 * inch, 3.6 * inch], style=TableStyle([
            ("FONT", (0, 0), (-1, -1), "Helvetica", 9), ("FONT", (0, 0), (-1, 0), "Helvetica-Bold", 9),
            ("BOX", (0, 0), (-1, -1), 0.6, colors.black), ("INNERGRID", (0, 0), (-1, -1), 0.3, colors.grey),
            ("VALIGN", (0, 0), (-1, -1), "TOP")])),
        Spacer(1, 8),
        Paragraph("This is important tax information and is being furnished to the Internal Revenue Service. "
                  "If you are required to file a return, a negligence penalty or other sanction may be imposed on "
                  "you if this income is taxable and the IRS determines that it has not been reported.", SMALL),
        Spacer(1, 10),
    ]

    ordinary = sum(d[1] for d in DIVIDENDS)
    qualified = sum(d[2] for d in DIVIDENDS)
    cap_gain = sum(d[3] for d in DIVIDENDS)
    foreign = sum(d[4] for d in DIVIDENDS)
    s += [Paragraph("<b>2025 1099-DIV</b> — Dividends and Distributions (OMB No. 1545-0110)", STYLES["Heading4"]),
          Table([["Box", "Description", "Amount"],
                 ["1a", "Total ordinary dividends", money(ordinary)],
                 ["1b", "Qualified dividends", money(qualified)],
                 ["2a", "Total capital gain distributions", money(cap_gain)],
                 ["3", "Nondividend distributions", money(0)],
                 ["4", "Federal income tax withheld", money(0)],
                 ["5", "Section 199A dividends", money(0)],
                 ["7", "Foreign tax paid", money(foreign)],
                 ["8", "Foreign country or U.S. possession", "VARIOUS"]],
                colWidths=[0.6 * inch, 4.6 * inch, 2.0 * inch], style=LEFT_DESCRIPTION),
          Spacer(1, 10),
          Paragraph("<b>2025 1099-INT</b> — Interest Income (OMB No. 1545-0112)", STYLES["Heading4"]),
          Table([["Box", "Description", "Amount"],
                 ["1", "Interest income (FDIC-insured bank sweep)", money(BROKERAGE_INTEREST)],
                 ["4", "Federal income tax withheld", money(0)]],
                colWidths=[0.6 * inch, 4.6 * inch, 2.0 * inch], style=LEFT_DESCRIPTION),
          Spacer(1, 10)]

    def totals(term):
        rows = [r for r in SALES if r[8] == term]
        proceeds, cost, wash = (sum(r[i] for r in rows) for i in (5, 6, 7))
        return proceeds, cost, wash, proceeds - cost + wash

    st, lt = totals("short"), totals("long")
    s += [Paragraph("<b>2025 1099-B</b> — Proceeds from Broker and Barter Exchange Transactions — Summary "
                    "(OMB No. 1545-0715)", STYLES["Heading4"]),
          Table([["Term / Form 8949 box", "Proceeds", "Cost basis", "Wash sale loss\ndisallowed", "Net gain or (loss)"],
                 ["Short-term, basis reported to IRS (Box A)", *map(money, st)],
                 ["Long-term, basis reported to IRS (Box D)", *map(money, lt)],
                 ["Total", *map(money, (st[i] + lt[i] for i in range(4)))]],
                colWidths=[2.6 * inch, 1.1 * inch, 1.1 * inch, 1.1 * inch, 1.3 * inch],
                style=TableStyle(GRID.getCommands() + [("FONT", (0, 3), (-1, 3), "Helvetica-Bold", 8)])),
          Spacer(1, 6),
          Paragraph("Federal income tax withheld (box 4): 0.00. All sales are of covered securities; cost basis "
                    "has been reported to the IRS (box 12 checked). See the detail that follows.", SMALL),
          PageBreak()]

    s += [Paragraph("<b>2025 1099-B Detail</b> — Account 4471-2231", STYLES["Heading3"])]
    for term, box, heading in (("short", "A", "SHORT-TERM TRANSACTIONS FOR COVERED TAX LOTS"),
                               ("long", "D", "LONG-TERM TRANSACTIONS FOR COVERED TAX LOTS")):
        rows = [cells(["1a Description (Symbol)", "Qty", "1b Date acquired", "1c Date sold", "1d Proceeds",
                       "1e Cost basis", "1g Wash sale loss disallowed", "Gain or (loss)", "Code"], HEAD, wrap=9)]
        for desc, sym, qty, acq, sold, proceeds, cost, wash, t in SALES:
            if t == term:
                rows.append(cells([f"{desc} ({sym})", str(qty), acq, sold, money(proceeds), money(cost),
                             money(wash) if wash else "", money(proceeds - cost + wash), "W" if wash else ""]))
        tot = totals(term)
        rows.append(["Total", "", "", "", money(tot[0]), money(tot[1]), money(tot[2]), money(tot[3]), ""])
        s += [Paragraph(f"{heading} — Report on Form 8949, Part {'I' if term == 'short' else 'II'}, "
                        f"with Box {box} checked. Box 2: {'Short' if term == 'short' else 'Long'}-term gain or loss; "
                        f"Box 5: covered security; Box 6: gross proceeds; Box 12: basis reported to IRS.", SMALL),
              Spacer(1, 3),
              Table(rows, colWidths=[1.9 * inch, 0.4 * inch, 0.75 * inch, 0.75 * inch, 0.8 * inch, 0.8 * inch,
                                     0.85 * inch, 0.75 * inch, 0.45 * inch], repeatRows=1,
                    style=TableStyle(GRID.getCommands() + [("FONT", (0, -1), (-1, -1), "Helvetica-Bold", 8)])),
              Spacer(1, 12)]
    s += [Paragraph("<b>Notes</b>", STYLES["Heading4"]),
          Paragraph("W — Wash sale. The loss on the 08/04/2025 sale of ACME ROBOTICS INC was disallowed because "
                    "substantially identical shares were purchased on 08/22/2025. The disallowed loss of 1,040.00 "
                    "was added to the cost basis of the replacement shares, and their date acquired was adjusted to "
                    "include the holding period of the shares sold. Both adjustments are reflected above.", SMALL),
          Spacer(1, 6),
          Paragraph("<b>2025 Dividend Detail</b> (supplemental — not furnished to the IRS)", STYLES["Heading4"]),
          Table([["Security", "Ordinary (1a)", "Qualified (1b)", "Cap. gain distr. (2a)", "Foreign tax (7)"]]
                + [cells([d[0], *map(money, d[1:])]) for d in DIVIDENDS],
                colWidths=[2.9 * inch, 1.1 * inch, 1.1 * inch, 1.2 * inch, 1.0 * inch], style=GRID)]
    doc.build(s)


def daycare_statement(out: Path) -> None:
    doc = SimpleDocTemplate(str(out), pagesize=letter, leftMargin=0.9 * inch, rightMargin=0.9 * inch,
                            topMargin=0.8 * inch, title="Little Sprouts 2025 Childcare Payment Statement")
    months = ["January", "February", "March", "April", "May", "June", "July", "August", "September",
              "October", "November", "December"]
    rows = [["Month", "Service", "Paid on", "Amount"]] + [
        [m, "Full-time preschool (3s room)", f"{i + 1:02d}/01/2025", money(DAYCARE_MONTHLY)] for i, m in enumerate(months)
    ] + [["Total paid in 2025", "", "", money(DAYCARE_MONTHLY * 12)]]
    doc.build([
        Paragraph("<b>Little Sprouts Early Learning Center, LLC</b>", STYLES["Title"]),
        Paragraph("2215 Larkspur Avenue · Denver, CO 80205 · (303) 555-0187", STYLES["Normal"]),
        Paragraph("Federal Employer Identification Number: 00-9753108", STYLES["Normal"]),
        Spacer(1, 16),
        Paragraph("<b>2025 Annual Childcare Payment Statement</b>", STYLES["Heading2"]),
        Paragraph(f"Statement date: 01/15/2026<br/>Parent/guardian: {JANE['first']} {JANE['last']}, "
                  f"{JANE['street']}, {JANE['city_state_zip']}<br/>Child: Leo Doe (date of birth 05/14/2022)",
                  STYLES["Normal"]),
        Spacer(1, 12),
        Table(rows, colWidths=[1.4 * inch, 2.6 * inch, 1.1 * inch, 1.2 * inch],
              style=TableStyle(GRID.getCommands() + [("ALIGN", (1, 1), (2, -1), "LEFT"),
                                                     ("FONT", (0, -1), (-1, -1), "Helvetica-Bold", 8)])),
        Spacer(1, 12),
        Paragraph("This statement lists payments received from the parent/guardian named above for childcare "
                  "services provided in calendar year 2025. Please keep it for your records; you may need our "
                  "name, address, and EIN for IRS Form 2441 or your dependent care FSA claims.", STYLES["Normal"]),
        Spacer(1, 30),
        Paragraph("Maria Alvarez, Center Director", STYLES["Normal"]),
    ])


if __name__ == "__main__":
    main()
