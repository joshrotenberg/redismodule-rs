// The harness is compiled only by this isolated package's tests.
#[cfg(test)]
#[path = "../shared.rs"]
mod shared;

#[cfg(test)]
#[path = "../cases.rs"]
mod cases;

#[cfg(test)]
mod tests;
