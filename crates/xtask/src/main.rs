// SPDX-License-Identifier: Apache-2.0
mod isolation;
use std::{collections::BTreeMap, fs, path::PathBuf};
struct Requirement {
    text: String,
    classification: String,
    scope: String,
}
fn classification_label(value: &str) -> Result<&'static str, Box<dyn std::error::Error>> {
    match value {
        "core" => Ok("共通基盤"),
        "optional" => Ok("公開の任意機能"),
        "host" => Ok("利用側の責任"),
        _ => Err(format!("unknown classification: {value}").into()),
    }
}
fn read_catalog(text: &str) -> Result<BTreeMap<String, Requirement>, Box<dyn std::error::Error>> {
    let catalog: serde_json::Value = serde_json::from_str(text)?;
    let mut rows = BTreeMap::new();
    for row in catalog.as_array().ok_or("catalog must be an array")? {
        let id = row["id"].as_str().ok_or("missing requirement id")?;
        let requirement = row["requirement"]
            .as_str()
            .ok_or("missing requirement text")?;
        let classification = row["classification"]
            .as_str()
            .ok_or("missing classification")?;
        classification_label(classification)?;
        let scope = row["scope"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("missing classification scope")?;
        if requirement.trim().is_empty()
            || rows
                .insert(
                    id.to_owned(),
                    Requirement {
                        text: requirement.to_owned(),
                        classification: classification.to_owned(),
                        scope: scope.to_owned(),
                    },
                )
                .is_some()
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
        Some("isolation-tests") | Some("isolation-results") => {
            let args: Vec<_> = std::env::args().collect();
            let suite = args.get(2).ok_or("missing suite")?;
            let feature = args.get(3).ok_or("missing feature")?;
            if args[1] == "isolation-tests" {
                for name in isolation::names(suite, feature)? {
                    println!("{name}");
                }
            } else {
                isolation::verify(
                    suite,
                    feature,
                    std::path::Path::new(args.get(4).ok_or("missing result directory")?),
                )?;
            }
        }
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
                "ContinuationPolicy",
                "CompactionPolicy",
                "RunHandle",
                "ToolContext",
                "RecoveryProof",
                "PreparationContext",
                "PhysicalSendObservation",
                "PhysicalSendEvidence",
                "ToolDescriptor",
                "ModelRequest",
                "ModelReply",
                "ValidationReceipt",
                "Diagnostic",
                "JsonPosition",
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
                "VAL-02", "VAL-04", "VAL-05", "VAL-06", "A12", "A19", "A21", "A22", "SBX-01", "A04",
                "SBX-08",
            ];
            let unstarted = ["OPS-05", "F06"];
            let mut counts = BTreeMap::new();
            let mut classified = BTreeMap::new();
            let mut out = String::from(
                "# 要件追跡（134件）\n\n[要件正本](../requirements/catalog.json)の基本要件81件、FORK 10件、受入シナリオA 28件、採用条件F 6件、利用シナリオU 9件。状態は**要件全体**の最も保守的な到達点。設計済みの行にも部分実装/試験があるが、未検証の全件達成を意味しない。実装済み・Linux未実行は関連Linux受入が未完了であり、本番運用の受入済みを意味しない。検証済みはfake/in-memory・local FS・指定テストDBの契約試験の範囲。分類と責任範囲はcatalogの正本から表示する。共通機構を利用側へ移す分類ではない。実サービス・Linux実Sandbox・電源断の耐久保証ではない。未着手行にも次段階のtrait設計がある。\n\n生成: `cargo run -p xtask -- trace`。同一仕様を複製する目的ではなく、repo内の要件正本の全IDを欠落なく追跡する。\n\n| ID | 要件/受入条件 | 分類 | 責任範囲 | 設計箇所 | 実装 | テスト/残課題 | 状態 |\n|---|---|---|---|---|---|---|---|\n",
            );
            for (id, requirement) in rows {
                let text = requirement.text;
                let class = classification_label(&requirement.classification)?;
                let scope = requirement.scope;
                let status =
                    if verified.contains(&id.as_str()) && requirement.classification != "host" {
                        "検証済み"
                    } else if ["REC-05", "REC-06", "SBX-07"].contains(&id.as_str()) {
                        "実装済み・Linux未実行"
                    } else if unstarted.contains(&id.as_str()) {
                        "未着手"
                    } else {
                        "設計済み"
                    };
                *counts.entry(status).or_insert(0usize) += 1;
                *classified.entry((class, status)).or_insert(0usize) += 1;
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
                let design = if id == "RUN-05" {
                    "[providers](providers.md), [images](images.md), [budget](budget.md), [api](api.md)"
                } else if id == "REC-08" {
                    "[postgres](postgres.md#host-owned-transaction), [api](api.md), [execution-boundary](execution-boundary.md)"
                } else if budget_scope {
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
                    ("利用側の配布・適合確認", "公開build/package手段を利用した実利用先への反映・業務適合は未検証")
                } else if ["REC-05", "REC-06", "SBX-07"].contains(&id.as_str()) {
                    (
                        "execution/{controller,linux}.rs; postgres/src/execution.rs; runtrell-exec-limit",
                        "事前ResourcePlan・専用subtree・tmpfs source・boot/mount namespace・inode照合。precreation_intent_is_discoverable_without_resource_identities、precreation_recovery_confirms_absence_and_reclaims_idempotently、mount_provenance_rejects_foreign_source_type_and_stacked_mountsはmacOS成功。linux_acquisition_crash_after_cgroup / after_mount / before_identity_recordはCI必須・Linux未実行。Memory/Pg台帳と予約/精算の既存契約を維持。末尾rt-4b参照",
                    )
                } else if id == "RUN-05" {
                    (
                        "api.rs::ModelPort/Usage; images.rs::ImageModel/ImageTool; attachments.rs; runtrell-providers（既定featureなし、openai/gemini/openrouter、GeminiImages）",
                        "providers/tests/http.rs: fake HTTP通常driver画像wire、生成/参照編集、汎用添付URL/inlineの送信時解決・署名URL更新、サイズ/digest/能力拒否、Debug/trace非出力、実Pg要求/checkpoint非保存・Unknown再開・保存結果再取得で原本解決0、送信前解決失敗の予約精算、DocumentViewのPDFページ選択→添付→送信・Workspace/Artifact固定版。testkit::image_contracts: Memory/Pg通常loop生成→保存→再入力→編集、保存結果復旧・1送信/精算額・課金済み不正画像・usage不足。PNG構造検査のみ、mask・実Provider・remote lookup/冪等保証は未対応/未検証。要件全体の状態は昇格しない",
                    )
                } else if id == "REC-08" {
                    (
                        "postgres/src/engine.rs::{start_in,create_account_in,create_account_with_policy_in,terminate_in,finalize_in}; persistence.rs::tx_in",
                        "postgres/tests/transaction.rs: host SQLとRun/口座のcommit・rollback・SQL失敗・同値受付/異値拒否・共有口座・親子Run・終端/後続Run境界・別process再open。耐久成功はhostのouter commit後。電源断/failoverは未検証",
                    )
                } else if ["RUN-06", "RUN-07", "A11", "A19", "VAL-06"].contains(&id.as_str()) {
                    (
                        "driver.rs / driver/context.rs / schema.rs / api.rs / ContextStore / Memory・Pg checkpoint",
                        "testkit/tests/repair.rs、driver_loop.rs、共有driver_contractsをMemory/Pgで実行。JSON/schema位置・原因・hint、receipt.repairable、明示上限・既定上限なし、保存/認可/証跡失敗の分離。通常loopのmax_tokens追加予約・精算、閾値で自動圧縮・元履歴保持・独立Pg poolからsummary再開。実Provider未実行",
                    )
                } else if budget_scope {
                    (
                        "budget.rs / driver.rs / memory.rs / postgres engine・persistence",
                        "testkit::budget_contractsをMemory/Pgで共通実行。軸/警告/方針/無料確定/不明予約/画像課金/attempt精算/優先度。既存fences/recovery/retryも維持。実サービスでの価格・使用量の正しさはhost責任",
                    )
                } else if execution_scope {
                    (
                        "execution/{mod,controller,process,tool}.rs; standard_tools.rs; postgres/src/execution.rs; runtrell-exec-limit",
                        "runtrell/tests/execution.rs; testkit/tests/{execution,standard_tools}.rs; Memory/Pg共通execution_contracts。Pg台帳再open・新backend client回収/冪等性、registry喪失Unknown、拒否/予約/CAS/CPU。macOS実隔離はsandbox外の過去報告のみ。LinuxBackend実装・実体再識別の実隔離試験は変更後のHosted実行未確認。execution.md参照",
                    )
                } else if data_scope {
                    (
                        "data.rs / data/local.rs / file_tools.rs / standard_tools.rs / driver.rs / schema.rs / assembly.rs",
                        "runtrell/tests/{data,backend_contract,schema}.rs・testkit/tests/{data_tools,repair,assembly,standard_tools}.rs。U05のcache/credential、A17の実Sandbox喪失、A26後半、DATA-06の運用/障害試験は残る。workspace.md参照",
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
                    "| {id} | {text} | {class} | {scope} | {design} | {implementation} | {tests} | {status} |\n"
                ));
            }
            let mut summary = String::from(
                "分類は責任の配置、状態は要件全体の到達点であり独立した軸。公開ソースに存在することや試験件数は、134要件の全達成を意味しない。利用側の責任のfake接続試験は、実hostでの業務受入の証明ではない。\n\n| 分類 | 検証済み | 実装済み・Linux未実行 | 設計済み | 未着手 | 合計 |\n|---|---:|---:|---:|---:|---:|\n",
            );
            for class in ["共通基盤", "公開の任意機能", "利用側の責任"] {
                let values: Vec<usize> = ["検証済み", "実装済み・Linux未実行", "設計済み", "未着手"]
                    .iter()
                    .map(|status| *classified.get(&(class, *status)).unwrap_or(&0))
                    .collect();
                summary.push_str(&format!(
                    "| {class} | {} | {} | {} | {} | {} |\n",
                    values[0],
                    values[1],
                    values[2],
                    values[3],
                    values.iter().sum::<usize>()
                ));
            }
            summary.push_str(&format!("| 全体 | {} | {} | {} | {} | 134 |\n", counts["検証済み"], counts["実装済み・Linux未実行"], counts["設計済み"], counts["未着手"]));
            summary.push_str("\n公開範囲: 共通実行基盤と選択可能な公開tool/backend/adapter。業務接続・設定と業務ロジックはhost側の別責務。方針型と共通実装はRuntrell、具体値はhostが供給する。ファイル操作・検索・画像生成/編集・Python・標準モデル接続等の再利用可能な実装も任意機能として扱い、全Runへ強制しない。この分類はそれら全ての実装完了を主張しない。\n\n試験件数: CIの各feature・suiteのlibtest結果を実行時の正本とする（同じ試験のfeature別実行を独立要件数へ加算しない）。Linux実隔離はintegration default 9件／python 10件と、test-only取得crash unit 3件（両feature）を必須とし、macOS実隔離は2件を名前とok結果で照合する。Hostedの成功はworkflow runの固定commit・runner・結果を確認して初めて記録する。変更後のHosted実行は未確認。本番構成・実provider接続・電源断・failover・負荷性能は未検証。\n");
            out.push_str("\n## 分類と受入範囲の集計\n\n");
            out.push_str(&summary);
            let readme_path = root.join("README.md");
            let readme = fs::read_to_string(&readme_path)?;
            let (before, rest) = readme
                .split_once("<!-- requirement-summary -->")
                .ok_or("missing README summary start")?;
            let (_, after) = rest
                .split_once("<!-- /requirement-summary -->")
                .ok_or("missing README summary end")?;
            fs::write(
                readme_path,
                format!(
                    "{before}<!-- requirement-summary -->\n{summary}<!-- /requirement-summary -->{after}"
                ),
            )?;
            out.push_str("\n## 状態別集計\n\n");
            for (status, count) in counts {
                out.push_str(&format!("- {status}: {count}\n"));
            }
            out.push_str("\n次段階: LinuxBackendの実隔離・耐久的な実体再識別と回収のHosted実証、実プロセス障害・stream/隔離試験、保存運用・業務owner接続・配布適合。Workspaceの提供範囲と制約はworkspace.md参照。2aのin-memory試験を耐久DBの検証済みへ読み替えない。\n");
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
        _ => return Err("usage: cargo run -p xtask -- trace|hashes|api-doc|isolation-tests <linux|macos> <default|python>|isolation-results <suite> <feature> <directory>".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_rejects_missing_or_unknown_classification_and_empty_scope() {
        let valid = include_str!("../../../docs/requirements/catalog.json");
        let mut rows: serde_json::Value = serde_json::from_str(valid).unwrap();
        rows[0].as_object_mut().unwrap().remove("classification");
        assert!(read_catalog(&rows.to_string()).is_err());
        rows[0]["classification"] = "business".into();
        assert!(read_catalog(&rows.to_string()).is_err());
        rows[0]["classification"] = "core".into();
        rows[0]["scope"] = "".into();
        assert!(read_catalog(&rows.to_string()).is_err());
    }

    #[test]
    fn catalog_has_exactly_the_required_ids() {
        let rows = read_catalog(include_str!("../../../docs/requirements/catalog.json")).unwrap();
        assert_eq!(rows.len(), 134);
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/requirements/catalog.json")).unwrap();
        assert!(
            catalog
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["classification"].is_string())
        );
        assert!(rows.values().all(|r| !r.text.trim().is_empty()));
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
