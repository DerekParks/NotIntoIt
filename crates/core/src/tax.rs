//! Form 1040 line 16 tax: the Tax Table (under $100,000), the Tax Computation
//! Worksheet, and the Qualified Dividends and Capital Gain Tax Worksheet.

use std::fmt;

use crate::calc::round_half_up;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilingStatus {
    Single,
    MarriedJoint,
    MarriedSeparate,
    HeadOfHousehold,
    QualifyingSurvivingSpouse,
}

impl FilingStatus {
    pub fn parse(s: &str) -> Option<Self> {
        let key: String = s
            .to_ascii_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        Some(match key.as_str() {
            "single" | "s" => Self::Single,
            "mfj" | "marriedfilingjointly" | "marriedjoint" | "joint" => Self::MarriedJoint,
            "mfs" | "marriedfilingseparately" | "marriedseparate" | "separate" => Self::MarriedSeparate,
            "hoh" | "headofhousehold" => Self::HeadOfHousehold,
            "qss" | "qualifyingsurvivingspouse" | "qualifyingwidower" | "qw" => Self::QualifyingSurvivingSpouse,
            _ => return None,
        })
    }

    fn column(self) -> usize {
        match self {
            Self::Single => 0,
            Self::MarriedJoint | Self::QualifyingSurvivingSpouse => 1,
            Self::MarriedSeparate => 2,
            Self::HeadOfHousehold => 3,
        }
    }
}

impl fmt::Display for FilingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Single => "Single",
            Self::MarriedJoint => "Married filing jointly",
            Self::MarriedSeparate => "Married filing separately",
            Self::HeadOfHousehold => "Head of household",
            Self::QualifyingSurvivingSpouse => "Qualifying surviving spouse",
        })
    }
}

/// One tax year's inflation-adjusted figures, per filing-status column
/// (single, joint/QSS, separate, head of household).
pub struct YearFigures {
    pub year: u16,
    pub source: &'static str,
    /// Upper bounds of the 10/12/22/24/32/35% brackets; 37% above the last.
    brackets: [[f64; 6]; 4],
    max_zero_rate: [f64; 4],
    max_fifteen_rate: [f64; 4],
    pub standard_deduction: [f64; 4],
}

const RATES: [f64; 7] = [0.10, 0.12, 0.22, 0.24, 0.32, 0.35, 0.37];

pub const YEARS: &[YearFigures] = &[
    YearFigures {
        year: 2024,
        source: "Rev. Proc. 2023-34",
        brackets: [
            [11_600.0, 47_150.0, 100_525.0, 191_950.0, 243_725.0, 609_350.0],
            [23_200.0, 94_300.0, 201_050.0, 383_900.0, 487_450.0, 731_200.0],
            [11_600.0, 47_150.0, 100_525.0, 191_950.0, 243_725.0, 365_600.0],
            [16_550.0, 63_100.0, 100_500.0, 191_950.0, 243_700.0, 609_350.0],
        ],
        max_zero_rate: [47_025.0, 94_050.0, 47_025.0, 63_000.0],
        max_fifteen_rate: [518_900.0, 583_750.0, 291_850.0, 551_350.0],
        standard_deduction: [14_600.0, 29_200.0, 14_600.0, 21_900.0],
    },
    YearFigures {
        year: 2025,
        source: "Rev. Proc. 2024-40; standard deduction per 26 U.S.C. § 63(c)(7) as amended by Pub. L. 119-21",
        brackets: [
            [11_925.0, 48_475.0, 103_350.0, 197_300.0, 250_525.0, 626_350.0],
            [23_850.0, 96_950.0, 206_700.0, 394_600.0, 501_050.0, 751_600.0],
            [11_925.0, 48_475.0, 103_350.0, 197_300.0, 250_525.0, 375_800.0],
            [17_000.0, 64_850.0, 103_350.0, 197_300.0, 250_500.0, 626_350.0],
        ],
        max_zero_rate: [48_350.0, 96_700.0, 48_350.0, 64_750.0],
        max_fifteen_rate: [533_400.0, 600_050.0, 300_000.0, 566_700.0],
        standard_deduction: [15_750.0, 31_500.0, 15_750.0, 23_625.0],
    },
];

pub fn figures(year: u16) -> Option<&'static YearFigures> {
    YEARS.iter().find(|y| y.year == year)
}

impl YearFigures {
    pub fn standard_deduction(&self, status: FilingStatus) -> f64 {
        self.standard_deduction[status.column()]
    }

    /// Exact bracket tax on `income`, before any table rounding.
    fn bracket_tax(&self, status: FilingStatus, income: f64) -> f64 {
        let bounds = &self.brackets[status.column()];
        let mut tax = 0.0;
        let mut lower = 0.0;
        for (i, rate) in RATES.iter().enumerate() {
            let upper = bounds.get(i).copied().unwrap_or(f64::INFINITY);
            tax += (income.min(upper) - lower).max(0.0) * rate;
            lower = upper;
        }
        tax
    }

    /// Tax from the Tax Table (income under $100,000, taxed at the row's
    /// midpoint, rounded to whole dollars) or the Tax Computation Worksheet.
    pub fn table_tax(&self, status: FilingStatus, taxable_income: f64) -> TableTax {
        let x = taxable_income.max(0.0);
        if x >= 100_000.0 {
            return TableTax {
                tax: round_half_up(self.bracket_tax(status, x), 2),
                row: None,
            };
        }
        let (lo, hi) = tax_table_row(x);
        let tax = round_half_up(self.bracket_tax(status, (lo + hi) / 2.0), 0);
        TableTax {
            tax,
            row: Some((lo, hi)),
        }
    }
}

pub struct TableTax {
    pub tax: f64,
    /// The Tax Table's "At least / But less than" row, when the table applies.
    pub row: Option<(f64, f64)>,
}

fn tax_table_row(x: f64) -> (f64, f64) {
    let (base, width) = match x {
        x if x < 5.0 => return (0.0, 5.0),
        x if x < 15.0 => return (5.0, 15.0),
        x if x < 25.0 => return (15.0, 25.0),
        x if x < 3_000.0 => (0.0, 25.0),
        _ => (0.0, 50.0),
    };
    let lo = base + ((x - base) / width).floor() * width;
    (lo, lo + width)
}

/// Line 16 inputs for the Qualified Dividends and Capital Gain Tax Worksheet.
pub struct Line16Input {
    pub taxable_income: f64,
    pub qualified_dividends: f64,
    /// Smaller of Schedule D lines 15 and 16 (or Form 1040 line 7a without Schedule D); 0 if a loss.
    pub net_capital_gain: f64,
}

pub struct Line16 {
    pub tax: f64,
    /// Worksheet lines as (line label, amount), for showing the work.
    pub lines: Vec<(String, f64)>,
    pub method: &'static str,
}

pub fn line16(fig: &YearFigures, status: FilingStatus, input: &Line16Input) -> Line16 {
    let ti = input.taxable_income.max(0.0);
    let qd = input.qualified_dividends.max(0.0);
    let cg = input.net_capital_gain.max(0.0);
    if qd == 0.0 && cg == 0.0 {
        let t = fig.table_tax(status, ti);
        let method = if t.row.is_some() {
            "Tax Table"
        } else {
            "Tax Computation Worksheet"
        };
        let mut lines = vec![("Taxable income (line 15)".to_string(), ti)];
        if let Some((lo, hi)) = t.row {
            lines.push((format!("Tax Table row: at least {lo} but less than {hi}"), t.tax));
        }
        return Line16 {
            tax: round_half_up(t.tax, 0),
            lines,
            method,
        };
    }

    let c = status.column();
    let l1 = ti;
    let l2 = qd;
    let l3 = cg;
    let l4 = l2 + l3;
    let l5 = (l1 - l4).max(0.0);
    let l6 = fig.max_zero_rate[c];
    let l7 = l1.min(l6);
    let l8 = l5.min(l7);
    let l9 = l7 - l8;
    let l10 = l1.min(l4);
    let l11 = l9;
    let l12 = l10 - l11;
    let l13 = fig.max_fifteen_rate[c];
    let l14 = l1.min(l13);
    let l15 = l5 + l9;
    let l16 = (l14 - l15).max(0.0);
    let l17 = l12.min(l16);
    let l18 = round_half_up(l17 * 0.15, 2);
    let l19 = l9 + l17;
    let l20 = l10 - l19;
    let l21 = round_half_up(l20 * 0.20, 2);
    let l22 = fig.table_tax(status, l5).tax;
    let l23 = l18 + l21 + l22;
    let l24 = fig.table_tax(status, l1).tax;
    let l25 = l23.min(l24);
    let lines = [
        ("1 Taxable income", l1),
        ("2 Qualified dividends", l2),
        ("3 Net capital gain", l3),
        ("4 Add lines 2 and 3", l4),
        ("5 Subtract line 4 from line 1", l5),
        ("6 Maximum 0% rate amount", l6),
        ("7 Smaller of line 1 or 6", l7),
        ("8 Smaller of line 5 or 7", l8),
        ("9 Taxed at 0%", l9),
        ("10 Smaller of line 1 or 4", l10),
        ("11 Line 9", l11),
        ("12 Subtract line 11 from 10", l12),
        ("13 Maximum 15% rate amount", l13),
        ("14 Smaller of line 1 or 13", l14),
        ("15 Add lines 5 and 9", l15),
        ("16 Subtract line 15 from 14", l16),
        ("17 Smaller of line 12 or 16 (taxed at 15%)", l17),
        ("18 Line 17 × 15%", l18),
        ("19 Add lines 9 and 17", l19),
        ("20 Subtract line 19 from 10 (taxed at 20%)", l20),
        ("21 Line 20 × 20%", l21),
        ("22 Tax on line 5 (Tax Table or Tax Computation Worksheet)", l22),
        ("23 Add lines 18, 21, and 22", l23),
        ("24 Tax on line 1 (Tax Table or Tax Computation Worksheet)", l24),
        ("25 Smaller of line 23 or 24", l25),
    ]
    .into_iter()
    .map(|(label, v)| (label.to_string(), v))
    .collect();
    Line16 {
        tax: round_half_up(l25, 0),
        lines,
        method: "Qualified Dividends and Capital Gain Tax Worksheet",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FilingStatus::*;

    /// (at least, but less than, single, joint, separate, head of household)
    /// sampled from the official Tax Tables (i1040tt 2024; i1040gi 2025).
    const TABLE_2024: &[[u32; 6]] = &[
        [0, 5, 0, 0, 0, 0],
        [2975, 3000, 299, 299, 299, 299],
        [3000, 3050, 303, 303, 303, 303],
        [12250, 12300, 1241, 1228, 1241, 1228],
        [34200, 34250, 3875, 3643, 3875, 3776],
        [47200, 47250, 5443, 5203, 5443, 5336],
        [51300, 51350, 6345, 5695, 6345, 5828],
        [64600, 64650, 9271, 7291, 9271, 7577],
        [80950, 81000, 12868, 9253, 12868, 11174],
        [99950, 100000, 17048, 12101, 17048, 15354],
    ];
    const TABLE_2025: &[[u32; 6]] = &[
        [0, 5, 0, 0, 0, 0],
        [25, 50, 4, 4, 4, 4],
        [2975, 3000, 299, 299, 299, 299],
        [13900, 13950, 1433, 1393, 1433, 1393],
        [17000, 17050, 1805, 1703, 1805, 1703],
        [21300, 21350, 2321, 2133, 2321, 2219],
        [48700, 48750, 5634, 5370, 5634, 5507],
        [73000, 73050, 10980, 8286, 10980, 9241],
        [94800, 94850, 15776, 10902, 15776, 14037],
        [99950, 100000, 16909, 11823, 16909, 15170],
    ];

    #[test]
    fn reproduces_official_tax_table_rows() {
        for (year, rows) in [(2024, TABLE_2024), (2025, TABLE_2025)] {
            let fig = figures(year).unwrap();
            for row in rows {
                for (status, want) in [Single, MarriedJoint, MarriedSeparate, HeadOfHousehold]
                    .iter()
                    .zip(&row[2..])
                {
                    for income in [row[0] as f64, row[1] as f64 - 0.01] {
                        let got = fig.table_tax(*status, income);
                        assert_eq!(got.tax, *want as f64, "{year} {status} income {income}");
                        assert_eq!(got.row, Some((row[0] as f64, row[1] as f64)));
                    }
                }
            }
        }
    }

    #[test]
    fn bracket_bases_match_rev_proc_tables() {
        // "$X plus N% of the excess over" figures from the Rev. Proc. tables.
        let fig = figures(2025).unwrap();
        assert_eq!(fig.bracket_tax(HeadOfHousehold, 64_850.0), 7_442.0);
        assert_eq!(fig.bracket_tax(HeadOfHousehold, 626_350.0), 187_031.5);
        assert_eq!(fig.bracket_tax(MarriedSeparate, 375_800.0), 101_077.25);
        assert_eq!(fig.bracket_tax(MarriedJoint, 751_600.0), 202_154.5);
        let fig = figures(2024).unwrap();
        assert_eq!(fig.bracket_tax(Single, 609_350.0), 183_647.25);
        assert_eq!(fig.bracket_tax(HeadOfHousehold, 609_350.0), 181_954.5);
    }

    #[test]
    fn computation_worksheet_above_100k() {
        // 2025 Tax Computation Worksheet, Section D: income × rate minus $6,825 (22%) or $8,892 (24%).
        let fig = figures(2025).unwrap();
        let t = fig.table_tax(HeadOfHousehold, 100_000.0);
        assert_eq!(t.row, None);
        assert_eq!(t.tax, 100_000.0 * 0.22 - 6_825.0);
        assert_eq!(
            fig.table_tax(HeadOfHousehold, 150_000.0).tax,
            150_000.0 * 0.24 - 8_892.0
        );
    }

    #[test]
    fn qualified_dividends_worksheet_jane_doe() {
        let fig = figures(2025).unwrap();
        let out = line16(
            fig,
            HeadOfHousehold,
            &Line16Input {
                taxable_income: 51_678.0,
                qualified_dividends: 413.0,
                net_capital_gain: 2_532.0,
            },
        );
        assert_eq!(out.tax, 5_507.0);
        assert_eq!(out.method, "Qualified Dividends and Capital Gain Tax Worksheet");
    }

    #[test]
    fn preferential_income_above_zero_bracket() {
        // Single 2025: $60,000 taxable, $20,000 long-term gain.
        // Ordinary $40,000 -> table row 40,000-40,050 midpoint; 0% band covers $8,350 of the gain;
        // the remaining $11,650 is taxed at 15%.
        let fig = figures(2025).unwrap();
        let out = line16(
            fig,
            Single,
            &Line16Input {
                taxable_income: 60_000.0,
                qualified_dividends: 0.0,
                net_capital_gain: 20_000.0,
            },
        );
        let ordinary = fig.table_tax(Single, 40_000.0).tax;
        assert_eq!(out.tax, round_half_up(ordinary + 11_650.0 * 0.15, 0));
        assert!(out.tax < fig.table_tax(Single, 60_000.0).tax);
    }

    #[test]
    fn statuses_parse() {
        assert_eq!(FilingStatus::parse("Head of household"), Some(HeadOfHousehold));
        assert_eq!(FilingStatus::parse("MFJ"), Some(MarriedJoint));
        assert_eq!(FilingStatus::parse("married filing separately"), Some(MarriedSeparate));
        assert_eq!(FilingStatus::parse("widow"), None);
    }
}
