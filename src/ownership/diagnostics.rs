//! Stable ownership diagnostic identifiers. Codes are compatibility surface:
//! wording and labels may improve, but a code's semantic category does not
//! change within an ownership-model version.

pub const USE_AFTER_MOVE: &str = "E0382";
pub const INVALIDATED_LOAN: &str = "E0401";
pub const BORROW_ESCAPE: &str = "E0402";
pub const SELF_BORROW: &str = "E0403";
pub const BORROWED_PARAMETER_CONSUMED: &str = "E0507";
pub const UNSAFE_SUSPENSION: &str = "E0728";

pub const CATALOG: &[(&str, &str)] = &[
    (USE_AFTER_MOVE, "use after move or before initialization"),
    (INVALIDATED_LOAN, "use of an invalidated loan"),
    (BORROW_ESCAPE, "borrow escapes its declared result contract"),
    (SELF_BORROW, "borrower is stored into its own origin"),
    (
        BORROWED_PARAMETER_CONSUMED,
        "borrow-by-default parameter is consumed",
    ),
    (UNSAFE_SUSPENSION, "loan storage cannot survive suspension"),
];
