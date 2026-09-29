use std::panic::{AssertUnwindSafe, catch_unwind};

use super::parse;

fn next(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed
}

fn corpus_case(index: usize, seed: &mut u64) -> String {
    match index % 8 {
        0 => String::new(),
        1 => "\n".to_owned(),
        2 => "---".to_owned(),
        3 => "[".to_owned(),
        4 => {
            let depth = 1 + (next(seed) % 64) as usize;
            format!("{}0{}", "[".repeat(depth), "]".repeat(depth))
        }
        5 => "key: 1\nkey: 2\n".to_owned(),
        6 => {
            if next(seed) & 1 == 0 {
                "yes\n".to_owned()
            } else {
                "on\n".to_owned()
            }
        }
        _ => "\t".repeat(1 + (next(seed) % 16) as usize),
    }
}

#[test]
fn parse_is_total_over_fixed_seed_corpus() {
    let mut seed = 0x5245_4649_4e45_4452;
    for index in 0..2_048 {
        let source = corpus_case(index, &mut seed);
        let result = catch_unwind(AssertUnwindSafe(|| parse(&source)));
        assert!(result.is_ok(), "parse panicked for corpus case {index}");
    }
}

#[test]
fn parse_retains_unsupported_items_and_keys() {
    let source = r#"opencollection: 1.0.0
info:
  name: Retention
bundled: true
x-unsupported: true
items:
  - info:
      type: note
      name: Ignored note
    x-item: keep
"#;
    let parsed = parse(source).expect("retention fixture should parse");
    assert!(parsed.collection().items.is_empty());
    let retained = parsed
        .to_yaml()
        .expect("retention fixture should serialize");
    assert!(retained.contains("x-unsupported: true"));
    assert!(retained.contains("Ignored note"));
    assert!(retained.contains("x-item: keep"));
}
