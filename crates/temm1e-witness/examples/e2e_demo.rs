//! E2E demo for Witness — runnable via `cargo run -p temm1e-witness --example e2e_demo`.
//!
//! This is a living walkthrough of every major Witness feature:
//!
//!  1. Auto-detect predicate sets for a workspace
//!  2. Build a Root Oath with Tier 0 postconditions
//!  3. Reject a lenient Oath via the Spec Reviewer
//!  4. Seal a rigorous Oath into a hash-chained Ledger
//!  5. Simulate an agent trajectory (honest, lying, fiction)
//!  6. Verify the Oath against each trajectory
//!  7. Compose an honest final reply on FAIL
//!  8. Verify Ledger integrity (hash chain intact)
//!  9. Mirror the live root hash to a watchdog anchor file
//! 10. Emit a summary readout to stdout
//!
//! The demo exits 0 on success, non-zero if any step diverges from the
//! Five Laws expected behavior.

use std::sync::Arc;

use temm1e_witness::{
    auto_detect::detect_active_sets,
    config::WitnessStrictness,
    ledger::Ledger,
    oath::seal_oath,
    predicate_sets::PredicateSetCatalog,
    types::{LedgerPayload, Oath, Predicate, VerdictOutcome},
    witness::Witness,
    WitnessError,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("════════════════════════════════════════════════════════════════");
    println!("  Witness E2E Demo — exercising every Phase 1 feature");
    println!("════════════════════════════════════════════════════════════════\n");

    let workspace = tempfile::tempdir()?;
    let workspace_path = workspace.path().to_path_buf();
    println!("workspace: {}", workspace_path.display());

    // ------------------------------------------------------------------
    // Step 1: Seed the workspace so auto-detect finds something.
    // ------------------------------------------------------------------
    tokio::fs::write(
        workspace_path.join("pyproject.toml"),
        "[project]\nname = \"demo\"\n",
    )
    .await?;
    tokio::fs::write(workspace_path.join("README.md"), "# demo\n").await?;

    let sets = detect_active_sets(&workspace_path);
    println!("\n[1] Auto-detected predicate sets: {:?}", sets);
    assert!(sets.contains(&"python".into()), "python set expected");

    let catalog = PredicateSetCatalog::defaults();
    println!(
        "[1] Default catalog exposes {} named sets",
        catalog.sets.len()
    );

    // ------------------------------------------------------------------
    // Step 2: Open an in-memory Ledger with a live-root mirror file.
    // ------------------------------------------------------------------
    let live_root_path = workspace_path.join(".witness_latest_root.hex");
    let ledger = Ledger::open("sqlite::memory:")
        .await?
        .with_live_root_path(live_root_path.clone());
    let witness = Arc::new(Witness::new(ledger.clone(), workspace_path.clone()));
    println!("\n[2] Opened Ledger + Witness");

    // ------------------------------------------------------------------
    // Step 3: Reject a lenient Oath (only a FileExists, no wiring/stub).
    // ------------------------------------------------------------------
    let lenient = Oath::draft("st-lenient", "root-1", "sess-demo", "implement compute_tax")
        .with_postcondition(Predicate::FileExists {
            path: workspace_path.join("compute_tax.py"),
        });
    match seal_oath(witness.ledger(), lenient).await {
        Err(WitnessError::LenientOath(reason)) => {
            println!(
                "[3] Spec Reviewer correctly rejected lenient Oath: {}",
                reason
            );
        }
        other => {
            eprintln!("[3] UNEXPECTED: lenient Oath was not rejected: {:?}", other);
            std::process::exit(2);
        }
    }

    // ------------------------------------------------------------------
    // Step 4: Seal a rigorous Root Oath.
    // ------------------------------------------------------------------
    let root_oath = Oath::draft("st-root", "root-1", "sess-demo", "implement compute_tax")
        .with_postcondition(Predicate::FileExists {
            path: workspace_path.join("compute_tax.py"),
        })
        .with_postcondition(Predicate::GrepCountAtLeast {
            pattern: "compute_tax".into(),
            path_glob: "*.py".into(),
            n: 2,
        })
        .with_postcondition(Predicate::GrepAbsent {
            pattern: "TODO|NotImplementedError".into(),
            path_glob: "*.py".into(),
        });
    let (sealed_root, root_entry_id) = seal_oath(witness.ledger(), root_oath).await?;
    println!(
        "\n[4] Root Oath sealed at entry_id {} (hash: {}...)",
        root_entry_id,
        &sealed_root.sealed_hash[..16]
    );

    // ------------------------------------------------------------------
    // Step 5: Simulate a lying agent (writes a stub).
    // ------------------------------------------------------------------
    tokio::fs::write(
        workspace_path.join("compute_tax.py"),
        "def compute_tax(amount):\n    raise NotImplementedError  # TODO\n",
    )
    .await?;
    println!("\n[5] Simulated LYING agent — stub with NotImplementedError");

    // ------------------------------------------------------------------
    // Step 6: Verify — should FAIL because of wiring + anti-stub predicates.
    // ------------------------------------------------------------------
    let verdict_lying = witness.verify_oath(&sealed_root).await?;
    println!(
        "[6] Verdict: {:?} ({}/{} predicates passed)",
        verdict_lying.outcome,
        verdict_lying.pass_count(),
        verdict_lying.total_count(),
    );
    assert_eq!(verdict_lying.outcome, VerdictOutcome::Fail);

    // ------------------------------------------------------------------
    // Step 7: Compose honest final reply (Block strictness).
    // ------------------------------------------------------------------
    let agent_reply = "Done! I implemented compute_tax.";
    let final_reply =
        witness.compose_final_reply(agent_reply, &verdict_lying, WitnessStrictness::Block);
    println!("\n[7] Final reply after rewriting (Block mode):");
    println!("{}", "─".repeat(72));
    println!("{}", final_reply);
    println!("{}", "─".repeat(72));

    // ------------------------------------------------------------------
    // Step 8: Fix the code and re-verify — should PASS.
    // ------------------------------------------------------------------
    tokio::fs::write(
        workspace_path.join("compute_tax.py"),
        "def compute_tax(amount: float) -> float:\n    return amount * 0.1\n\n\
         if __name__ == '__main__':\n    print(compute_tax(100.0))\n",
    )
    .await?;

    // A new subtask for the fix
    let fix_oath = Oath::draft(
        "st-fix",
        "root-1",
        "sess-demo",
        "implement compute_tax correctly",
    )
    .with_postcondition(Predicate::FileExists {
        path: workspace_path.join("compute_tax.py"),
    })
    .with_postcondition(Predicate::GrepCountAtLeast {
        pattern: "compute_tax".into(),
        path_glob: "*.py".into(),
        n: 2,
    })
    .with_postcondition(Predicate::GrepAbsent {
        pattern: "NotImplementedError|# TODO".into(),
        path_glob: "*.py".into(),
    });
    let (sealed_fix, _) = seal_oath(witness.ledger(), fix_oath).await?;
    let verdict_fixed = witness.verify_oath(&sealed_fix).await?;
    println!(
        "\n[8] After honest fix: verdict {:?} ({}/{} passed)",
        verdict_fixed.outcome,
        verdict_fixed.pass_count(),
        verdict_fixed.total_count(),
    );
    assert_eq!(verdict_fixed.outcome, VerdictOutcome::Pass);

    // ------------------------------------------------------------------
    // Step 9: Verify full Ledger integrity and inspect chain.
    // ------------------------------------------------------------------
    witness.ledger().verify_integrity().await?;
    let total = witness.ledger().count_total().await?;
    println!("\n[9] Ledger integrity verified. {} total entries.", total);

    let entries = witness.ledger().read_session("sess-demo").await?;
    println!("    Entries for sess-demo:");
    for e in &entries {
        let type_str = match &e.payload {
            LedgerPayload::OathSealed(_) => "OathSealed".to_string(),
            LedgerPayload::VerdictRendered(v) => format!("VerdictRendered({:?})", v.outcome),
            other => format!("{:?}", std::mem::discriminant(other)),
        };
        println!(
            "      #{:3} [{}] {}..{}  cost=${:.4}",
            e.entry_id,
            type_str,
            &e.entry_hash[..8],
            &e.entry_hash[e.entry_hash.len() - 8..],
            e.cost_usd,
        );
    }

    // ------------------------------------------------------------------
    // Step 10: Verify live-root mirror file was written.
    // ------------------------------------------------------------------
    let live_root = std::fs::read_to_string(&live_root_path)?;
    let live_root = live_root.trim();
    println!(
        "\n[10] Live root mirror file: {} ({} chars)",
        live_root_path.display(),
        live_root.len()
    );
    assert_eq!(live_root.len(), 64, "expected 64-char hex root hash");

    // ------------------------------------------------------------------
    // Step 11: Prove Law 5 — no files were deleted by Witness.
    // ------------------------------------------------------------------
    assert!(workspace_path.join("compute_tax.py").exists());
    assert!(workspace_path.join("README.md").exists());
    assert!(workspace_path.join("pyproject.toml").exists());
    println!("[11] Law 5 verified — all workspace files intact after Witness run");

    // ------------------------------------------------------------------
    // Summary
    // ------------------------------------------------------------------
    println!("\n════════════════════════════════════════════════════════════════");
    println!("  E2E DEMO COMPLETE");
    println!("════════════════════════════════════════════════════════════════");
    println!("  • Predicate sets auto-detected: {}", sets.len());
    println!("  • Lenient oath rejected: ✓");
    println!("  • Root oath sealed: ✓");
    println!(
        "  • Lying agent caught: ✓ (verdict={:?})",
        verdict_lying.outcome
    );
    println!(
        "  • Honest rewrite verified: ✓ (verdict={:?})",
        verdict_fixed.outcome
    );
    println!("  • Ledger integrity: ✓ ({} entries chained)", total);
    println!(
        "  • Watchdog root anchor mirror: ✓ ({} chars)",
        live_root.len()
    );
    println!("  • Law 5 (no file deletion): ✓");
    println!();
    Ok(())
}
