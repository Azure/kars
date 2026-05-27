#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# ═══════════════════════════════════════════════════════════════════
# kars Demo: Operation Claw Shield — Normal Workflow
# ═══════════════════════════════════════════════════════════════════
#
# Demonstrates normal multi-agent collaboration:
#   1. Contoso Bank agent generates a compliance report
#   2. Fabrikam Legal agent reviews it for regulatory compliance
#   3. Northwind Traders agent validates trade records against it
#
# Run this from the operator's machine (not inside a sandbox).
#
# Usage:
#   bash examples/demo-clawshield/normal-workflow.sh
#
# ═══════════════════════════════════════════════════════════════════

set -euo pipefail

CYAN='\033[0;36m'
GREEN='\033[0;32m'
BOLD='\033[1m'
NC='\033[0m'

step() {
    echo ""
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BOLD}  Step $1: $2${NC}"
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

# ═══════════════════════════════════════════════════════════════════
echo -e "${BOLD}${CYAN}"
echo "  ╔═══════════════════════════════════════════════════════════╗"
echo "  ║  kars Demo: Operation Claw Shield                   ║"
echo "  ║  Multi-Agent Compliance Workflow                         ║"
echo "  ╚═══════════════════════════════════════════════════════════╝"
echo -e "${NC}"
# ═══════════════════════════════════════════════════════════════════

step 0 "Verify all sandboxes are running"
echo ""
kubectl get karssandboxes -A -o wide 2>/dev/null || echo "(KarsSandbox CRDs — showing expected output)"
echo ""
echo -e "${GREEN}✅ Three company agents running in isolated namespaces${NC}"
echo "   • contoso-bank-agent     (enhanced — runc + seccomp)"
echo "   • fabrikam-legal-agent   (confidential — Kata VM)"
echo "   • northwind-trade-agent  (enhanced — runc + seccomp)"

step 1 "Contoso Bank: Generate Q4 compliance report"
echo ""
echo "Sending inference request to Contoso's sandbox..."
echo ""
echo '  POST http://localhost:11434/v1/chat/completions'
echo '  Model: gpt-4.1 (via Azure AI Foundry)'
echo '  Content Safety: ✅ enabled'
echo '  Prompt Shields: ✅ enabled'
echo '  Token Budget:   500,000/day (12,847 used)'
echo ""
echo -e "${GREEN}✅ Contoso agent generated Q4 compliance report${NC}"
echo "   Tokens used: 3,847 (input: 1,200 + output: 2,647)"
echo "   Content Safety: PASS (no harmful content)"
echo "   Prompt Shields: PASS (no injection detected)"

step 2 "Fabrikam Legal: Review compliance report"
echo ""
echo "Fabrikam's agent reviews the report in a Kata VM sandbox..."
echo ""
echo '  Runtime: kata-vm-isolation (dedicated kernel)'
echo '  Model: gpt-4.1 (via Azure AI Foundry)'
echo '  NetworkPolicy: legal-api.fabrikam.com, efts.sec.gov only'
echo ""
echo -e "${GREEN}✅ Fabrikam agent produced legal assessment${NC}"
echo "   Tokens used: 5,231 (input: 2,100 + output: 3,131)"
echo "   Content Safety: PASS"
echo "   Prompt Shields: PASS"
echo "   Kata VM: isolated execution ✅"

step 3 "Northwind Traders: Validate trade records"
echo ""
echo "Northwind's agent cross-references trade data..."
echo ""
echo '  Model: gpt-4.1 (via Azure AI Foundry)'
echo '  NetworkPolicy: trades.northwind.com, api.trade.gov only'
echo ""
echo -e "${GREEN}✅ Northwind agent validated trade compliance${NC}"
echo "   Tokens used: 2,456 (input: 900 + output: 1,556)"
echo "   Content Safety: PASS"
echo "   Prompt Shields: PASS"

step 4 "Operator dashboard"
echo ""
echo "Multi-tenant status:"
echo ""
printf "  %-25s %-12s %-15s %-12s %s\n" "SANDBOX" "PHASE" "ISOLATION" "TOKENS" "STATUS"
printf "  %-25s %-12s %-15s %-12s %s\n" "─────────────────────────" "────────────" "───────────────" "────────────" "──────"
printf "  %-25s %-12s %-15s %-12s %s\n" "contoso-bank-agent" "Running" "enhanced" "16,694/500K" "✅ healthy"
printf "  %-25s %-12s %-15s %-12s %s\n" "fabrikam-legal-agent" "Running" "confidential" "5,231/200K" "✅ healthy"
printf "  %-25s %-12s %-15s %-12s %s\n" "northwind-trade-agent" "Running" "enhanced" "10,690/300K" "✅ healthy"
echo ""
echo -e "${GREEN}${BOLD}All agents operating normally. No security events.${NC}"
echo ""
echo "Next: Run the attack simulation to demo kars's security:"
echo "  kars connect fabrikam-legal-agent --shell"
echo "  bash /sandbox/attack-simulation.sh"
