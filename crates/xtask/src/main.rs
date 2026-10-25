// SPDX-License-Identifier: Apache-2.0
use std::{collections::BTreeMap, fs, path::PathBuf};
fn read_catalog(text: &str) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let catalog: serde_json::Value = serde_json::from_str(text)?;
    let mut rows = BTreeMap::new();
    for row in catalog.as_array().ok_or("catalog must be an array")? {
        let id = row["id"].as_str().ok_or("missing requirement id")?;
        let requirement = row["requirement"]
            .as_str()
            .ok_or("missing requirement text")?;
        if requirement.trim().is_empty()
            || rows.insert(id.to_owned(), requirement.to_owned()).is_some()
        {
            return Err(format!("empty or duplicate requirement: {id}").into());
        }
    }
    let mut expected = Vec::new();
    for (prefix, count) in [
        ("RUN-", 8),
        ("TOOL-", 8),
        ("DATA-", 8),
        ("SBX-", 8),
        ("RES-", 7),
        ("PAR-", 6),
        ("REC-", 8),
        ("VAL-", 6),
        ("SEC-", 7),
        ("ARC-", 7),
        ("OPS-", 8),
        ("FORK-", 10),
        ("A", 28),
        ("F", 6),
        ("U", 9),
    ] {
        for number in 1..=count {
            expected.push(format!("{prefix}{number:02}"));
        }
    }
    expected.sort();
    if rows.keys().ne(expected.iter()) {
        return Err("catalog must contain exactly the 134 required IDs".into());
    }
    Ok(rows)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    match std::env::args().nth(1).as_deref() {
        Some("api-doc") => {
            let source = fs::read_to_string(root.join("crates/runtrell/src/api.rs"))?;
            let path = root.join("docs/design/api.md");
            let original = fs::read_to_string(&path)?;
            let mut out = original
                .split("<!-- generated-contracts -->")
                .next()
                .unwrap()
                .trim_end()
                .to_string();
            out.push_str("\n\n<!-- generated-contracts -->\n## Rust契約定義\n\n`cargo run -p xtask -- api-doc`で公開ソースから生成。型の全保存フィールド、Result/Errorとenumは[api.rs](../../crates/runtrell/src/api.rs)を参照する。以下は同じ正本の抜粋であり独立した仕様ではない。\n\n```rust\n");
            for name in [
                "RunSpec",
                "RunHandle",
                "ToolContext",
                "ToolDescriptor",
                "ModelRequest",
                "ModelReply",
                "ValidationReceipt",
            ] {
                let start = source
                    .find(&format!("pub struct {name} {{"))
                    .ok_or("missing public type")?;
                let tail = &source[start..];
                let end = tail.find("\n}").ok_or("unclosed type")? + 2;
                out.push_str(&tail[..end]);
                out.push_str("\n\n");
            }
            let start = source.find("pub trait Clock").ok_or("missing traits")?;
            let end = source
                .find("/// Content identity")
                .ok_or("missing trait boundary")?;
            out.push_str(&source[start..end]);
            out.push_str("```\n");
            fs::write(path, out)?;
        }
        Some("trace") => {
            let rows = read_catalog(&fs::read_to_string(
                root.join("docs/requirements/catalog.json"),
            )?)?;
            let verified = [
                "RUN-01", "RUN-03", "RUN-04", "TOOL-01", "TOOL-03", "TOOL-06", "ARC-02", "ARC-03",
                "ARC-04", "ARC-05", "FORK-01", "FORK-02", "FORK-03", "FORK-04", "FORK-05",
                "FORK-06", "FORK-07", "FORK-08", "FORK-09", "F01", "F02", "F04", "F05", "A01",
                "U01", "U02", "U03", "U04", "U06", "U09", "DATA-01", "DATA-02", "DATA-03",
                "DATA-04", "DATA-05", "DATA-07", "DATA-08", "TOOL-07", "TOOL-08", "VAL-01",
                "VAL-02", "VAL-04", "VAL-05", "A12", "A19", "A21", "A22", "SBX-01", "A04",
                "SBX-08",
            ];
            let unstarted = ["OPS-05", "F06"];
            let mut counts = BTreeMap::new();
            let mut out = String::from(
                "# 要件追跡（134件）\n\n[要件正本](../requirements/catalog.json)の基本要件81件、FORK 10件、受入シナリオA 28件、採用条件F 6件、利用シナリオU 9件。状態は**要件全体**の最も保守的な到達点。設計済みの行にも部分実装/試験があるが、未検証の全件達成を意味しない。検証済みはfake/in-memory・local FS・指定テストDBの契約試験の範囲。実サービス・Linux実Sandbox・電源断の耐久保証ではない。未着手行にも次段階のtrait設計がある。\n\n生成: `cargo run -p xtask -- trace`。同一仕様を複製する目的ではなく、repo内の要件正本の全IDを欠落なく追跡する。\n\n| ID | 要件/受入条件 | 設計箇所 | 実装 | テスト/残課題 | 状態 |\n|---|---|---|---|---|---|\n",
            );
            for (id, text) in rows {
                let status = if verified.contains(&id.as_str()) {
                    "検証済み"
                } else if unstarted.contains(&id.as_str()) {
                    "未着手"
                } else {
                    "設計済み"
                };
                *counts.entry(status).or_insert(0usize) += 1;
                let data_scope = id.starts_with("DATA")
                    || [
                        "TOOL-07", "TOOL-08", "VAL-01", "VAL-02", "VAL-04", "VAL-05", "RUN-06",
                        "A11", "A12", "A17", "A19", "A21", "A22", "A26", "U06", "U05",
                    ]
                    .contains(&id.as_str());
                let execution_scope = id.starts_with("SBX")
                    || [
                        "U08", "A03", "A04", "A07", "A08", "A13", "A14", "A18", "A22", "A27",
                        "A28", "RES-04", "RES-05", "RES-06", "RES-07", "REC-05", "REC-06",
                        "SEC-02", "SEC-03", "SEC-04", "SEC-06",
                    ]
                    .contains(&id.as_str());
                let budget_scope = [
                    "RES-01", "RES-02", "RES-03", "PAR-03", "PAR-05", "U09", "A06",
                ]
                .contains(&id.as_str());
                let design = if budget_scope {
                    "[budget](budget.md), [concurrency](concurrency.md), [api](api.md)"
                } else if execution_scope {
                    "[execution](execution.md), [workspace](workspace.md)"
                } else if data_scope {
                    "[workspace](workspace.md), [api](api.md)"
                } else if id.starts_with("PAR") || id.starts_with("RES") {
                    "[concurrency](concurrency.md)"
                } else if id.starts_with("REC") || id.starts_with('A') || id.starts_with('U') {
                    "[execution-boundary](execution-boundary.md), [failure-matrix](failure-matrix.md)"
                } else if id.starts_with("FORK") || id.starts_with('F') {
                    "[adoption](../adoption/reuse-map.md), [api](api.md)"
                } else {
                    "[api](api.md), [state](state.md)"
                };
                let (implementation, tests) = if status == "未着手" {
                    ("traitのみ/次段階", "本実装・対応試験は未着手")
                } else if budget_scope {
                    (
                        "budget.rs / driver.rs / memory.rs / postgres engine・persistence",
                        "testkit::budget_contractsをMemory/Pgで共通実行。軸/警告/方針/無料確定/不明予約/画像課金/attempt精算/優先度。既存fences/recovery/retryも維持。実サービスでの価格・使用量の正しさはhost責任",
                    )
                } else if execution_scope {
                    (
                        "execution/{mod,controller,process}.rs; postgres/src/execution.rs; runtrell-exec-limit",
                        "runtrell/tests/execution.rs; testkit/tests/execution.rs; Memory/Pg共通execution_contracts。Pg台帳再open・新backend client回収/冪等性、registry喪失Unknown、拒否/予約/CAS/CPU。macOS実隔離はsandbox外の明示実行時のみ確認との過去報告。Linux/実体再識別は未検証。execution.md参照",
                    )
                } else if data_scope {
                    (
                        "data.rs / data/local.rs / file_tools.rs / driver.rs / schema.rs / assembly.rs",
                        "runtrell/tests/{data,backend_contract,schema}.rs・testkit/tests/{data_tools,repair,assembly}.rs。U05のcache/credential、A17の実Sandbox喪失、A26後半、DATA-06の運用/障害試験は残る。workspace.md参照",
                    )
                } else if id.starts_with("FORK") || id == "F01" || id == "F02" {
                    (
                        "protocol + docs/adoption",
                        "継承334 unit/5 integration/53 doc、package/tree。運用方針は記録確認",
                    )
                } else if id.starts_with("ARC") || id == "A01" || id == "F04" || id == "F05" {
                    (
                        "runtrell + runtrell-testkit",
                        "workspace build/test、2接続例、protocol型非露出",
                    )
                } else if id.starts_with("U") {
                    match id.as_str() {
                        "U01" => ("Runtime/Validator/Finalizer", "runtime::u01_*"),
                        "U02" => ("Runtime/2 host tools", "runtime::u02_*"),
                        "U03" => (
                            "dispatch/lookup/Delivery",
                            "runtime::u03_*、recovery 10case",
                        ),
                        "U04" => (
                            "request重複排除/host効果key",
                            "runtime::u04_*、recovery::handoff",
                        ),
                        "U05" => (
                            "tenant/Context/budget/policy分離",
                            "runtime::u05_*。Workspace/cache/credential隔離は次段階",
                        ),
                        "U06" => ("receipt/revision CAS", "runtime::u06_*。Artifact版は次段階"),
                        "U07" => (
                            "trigger/sequence/context CAS",
                            "runtime::u07_*。耐久受付はDB adapterで検証",
                        ),
                        _ => (
                            "全祖先account予約/unknown保持",
                            "runtime::u09_*、budget_*、recovery/retry",
                        ),
                    }
                } else {
                    (
                        "api.rs / memory.rs / driver.rs（部分実装を含む）",
                        "testkit/tests/{runtime,recovery,retry}.rs、protocol継承。全要件未達は設計本文参照",
                    )
                };
                out.push_str(&format!(
                    "| {id} | {text} | {design} | {implementation} | {tests} | {status} |\n"
                ));
            }
            out.push_str("\n## 状態別集計\n\n");
            for (status, count) in counts {
                out.push_str(&format!("- {status}: {count}\n"));
            }
            out.push_str("- 実装済み（未検証のみ）: 0\n\n次段階: ExecutionBackendのLinux実隔離・backend別の安全な実体再識別、実プロセス障害・stream/隔離試験、保存運用・業務owner接続・配布適合。Workspaceの提供範囲と制約はworkspace.md参照。2aのin-memory試験を耐久DBの検証済みへ読み替えない。\n");
            let path = root.join("docs/design/requirements-trace.md");
            // Keep independently maintained evidence beyond the generated table.
            if let Ok(previous) = fs::read_to_string(&path)
                && let Some((_, evidence)) = previous.split_once("<!-- supplemental-evidence -->")
            {
                out.push_str("\n<!-- supplemental-evidence -->");
                out.push_str(evidence);
            }
            fs::write(path, out)?;
        }
        Some("hashes") => {
            use sha2::{Digest, Sha256};
            // Adopted files evolve after the snapshot, so `extracted_sha256` is a
            // historical record of the initial extraction. Current checks cover the
            // provenance header, and, given the pinned Rig checkout, the source hash.
            const ORIGIN: &str =
                "Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514)";
            const COPYRIGHT: &str = "Copyright (c) 2024, Playgrounds Analytics Inc.";
            let rig = std::env::var_os("RIG_SNAPSHOT").map(std::path::PathBuf::from);
            let digest = |path: &std::path::Path| -> Result<String, Box<dyn std::error::Error>> {
                Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
            };
            let mut checked = 0;
            let mut sources = 0;
            let adopted: serde_json::Value = serde_json::from_str(&fs::read_to_string(
                root.join("docs/adoption/snapshot-files.json"),
            )?)?;
            for row in adopted.as_array().ok_or("manifest must be array")? {
                let target = row["target"].as_str().ok_or("missing target")?;
                let source = row["source"].as_str().ok_or("missing source")?;
                let head: String = fs::read_to_string(root.join(target))?
                    .lines()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !head.contains(&format!("{ORIGIN}, {source}."))
                    || !head.contains(COPYRIGHT)
                    || !head.contains(
                        "inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.",
                    )
                {
                    return Err(format!("provenance header missing: {target}").into());
                }
                if let Some(rig) = &rig {
                    if Some(digest(&rig.join(source))?.as_str()) != row["source_sha256"].as_str() {
                        return Err(format!("Rig source hash mismatch: {source}").into());
                    }
                    sources += 1;
                }
                checked += 1;
            }
            let actual = format!(
                "{:x}",
                Sha256::digest(fs::read(root.join("LICENSES/MIT-Rig.txt"))?)
            );
            if actual != "42cf0f78132fe1de338cbff947bf5b9f7d2d8a377cb827c9df9e90b4b9499ef7" {
                return Err("MIT license hash mismatch".into());
            }
            println!(
                "Verified {checked} adopted-file provenance/license headers, {sources} Rig source hashes (set RIG_SNAPSHOT to check them), and the Rig MIT license; no manifests rewritten."
            );
        }
        _ => return Err("usage: cargo run -p xtask -- trace|hashes|api-doc".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_exactly_the_required_ids() {
        let rows = read_catalog(include_str!("../../../docs/requirements/catalog.json")).unwrap();
        assert_eq!(rows.len(), 134);
        assert!(rows.values().all(|text| !text.trim().is_empty()));
    }

    #[test]
    fn catalog_rejects_missing_unknown_duplicate_and_empty_requirements() {
        let valid = include_str!("../../../docs/requirements/catalog.json");
        let mut rows: serde_json::Value = serde_json::from_str(valid).unwrap();
        rows.as_array_mut().unwrap().pop();
        assert!(read_catalog(&rows.to_string()).is_err());
        assert!(read_catalog(&valid.replacen("A01", "A99", 1)).is_err());
        let mut rows: serde_json::Value = serde_json::from_str(valid).unwrap();
        let duplicate = rows[0].clone();
        rows.as_array_mut().unwrap().push(duplicate);
        assert!(read_catalog(&rows.to_string()).is_err());
        let mut rows: serde_json::Value = serde_json::from_str(valid).unwrap();
        rows[0]["requirement"] = serde_json::Value::String(String::new());
        assert!(read_catalog(&rows.to_string()).is_err());
    }
}
