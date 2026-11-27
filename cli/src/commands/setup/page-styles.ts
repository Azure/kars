// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const setupPageStyles = String.raw`
:root {
  color-scheme: light;
  --cp-bg: #f7f4ef;
  --cp-bg-elevated: #fcfbf8;
  --cp-surface: #ffffff;
  --cp-surface-soft: #f5f5f5;
  --cp-border: #dedede;
  --cp-border-strong: #919191;
  --cp-text: #242424;
  --cp-text-muted: #5c5c5c;
  --cp-text-soft: #6f6f6f;
  --cp-accent: #b11f4b;
  --cp-accent-hover: #9a1a41;
  --cp-accent-soft: rgba(177, 31, 75, 0.08);
  --cp-accent-fg: #ffffff;
  --cp-success: #16a34a;
  --cp-danger: #dc2626;
  --cp-warning: #f59e0b;
  --cp-link: #0078d4;
  --cp-shadow: 0 18px 48px rgba(0, 0, 0, 0.12);
  --cp-overlay: rgba(255, 255, 255, 0.8);
  --cp-panel: rgba(255, 255, 255, 0.86);
  --cp-panel-strong: rgba(255, 255, 255, 0.96);
  --cp-sheen: rgba(255, 255, 255, 0.55);
  --cp-highlight: rgba(177, 31, 75, 0.12);
}
html[data-theme="dark"] {
  color-scheme: dark;
  --cp-bg: #3d3b3a;
  --cp-bg-elevated: #343231;
  --cp-surface: #292929;
  --cp-surface-soft: #2e2e2e;
  --cp-border: #474747;
  --cp-border-strong: #5f5f5f;
  --cp-text: #dedede;
  --cp-text-muted: #919191;
  --cp-text-soft: #b0b0b0;
  --cp-accent: #fd8ea1;
  --cp-accent-hover: #fb7b91;
  --cp-accent-soft: rgba(253, 142, 161, 0.14);
  --cp-accent-fg: #1a1a1a;
  --cp-success: #4ade80;
  --cp-danger: #f87171;
  --cp-warning: #fbbf24;
  --cp-link: #4da6ff;
  --cp-shadow: 0 18px 48px rgba(0, 0, 0, 0.32);
  --cp-overlay: rgba(41, 41, 41, 0.88);
  --cp-panel: rgba(41, 41, 41, 0.72);
  --cp-panel-strong: rgba(41, 41, 41, 0.96);
  --cp-sheen: rgba(255, 255, 255, 0.04);
  --cp-highlight: rgba(253, 142, 161, 0.12);
}
/* Brand overrides reuse the unpublished kars-runtime.io design tokens. */
html[data-brand="kars"][data-theme="dark"] {
  --cp-bg: #090c10; --cp-bg-elevated: #10151c; --cp-surface: #141b24;
  --cp-surface-soft: #1b2430; --cp-border: #2a3442; --cp-border-strong: #526175;
  --cp-text: #f2f5f8; --cp-text-soft: #aebdce; --cp-text-muted: #90a2b8;
  --cp-accent: #c6f77e; --cp-accent-hover: #dbffa6; --cp-accent-fg: #132005;
  --cp-accent-soft: #1d2a16; --cp-bridge: #b3c4ff; --cp-bridge-soft: #1e263d;
}
html[data-brand="kars"][data-theme="light"] {
  --cp-bg: #f4f7fb; --cp-bg-elevated: #ffffff; --cp-surface: #ffffff;
  --cp-surface-soft: #eaf0f6; --cp-border: #d3dce7; --cp-border-strong: #8291a5;
  --cp-text: #101b2a; --cp-text-soft: #45586f; --cp-text-muted: #51647a;
  --cp-accent: #456919; --cp-accent-hover: #365612; --cp-accent-fg: #ffffff;
  --cp-accent-soft: #e6f2d7; --cp-bridge: #3b50a4; --cp-bridge-soft: #e9edff;
  --cp-success: #23723d; --cp-danger: #b91c1c; --cp-warning: #885500;
}
* { box-sizing: border-box; }
html { scroll-padding-block: 28px 112px; }
[hidden] { display: none !important; }
body { margin: 0; background: var(--cp-bg); color: var(--cp-text); font: 15px/1.6 "Segoe UI", Aptos, Calibri, -apple-system, BlinkMacSystemFont, sans-serif; -webkit-font-smoothing: antialiased; }
button,input,select { font: inherit; }
button,a,input,select { -webkit-tap-highlight-color: var(--cp-accent-soft); }
:focus-visible { outline: 2px solid var(--cp-accent); outline-offset: 4px; }
button { cursor: pointer; }
button:disabled { cursor: not-allowed; opacity: .5; }
a { color: var(--cp-accent); }
button,.button { display: inline-flex; align-items: center; justify-content: center; gap: 8px; padding: 12px 20px; border: 1px solid var(--cp-border-strong); border-radius: 10px; background: var(--cp-bg-elevated); color: var(--cp-text); font-weight: 600; text-decoration: none; transition: background-color 150ms, border-color 150ms, transform 120ms; }
button:hover:not(:disabled),.button:hover { border-color: var(--cp-accent); }
button:active:not(:disabled),.button:active { transform: translateY(1px); }
.primary { background: var(--cp-accent); color: var(--cp-accent-fg); border-color: var(--cp-accent); }
.primary:hover:not(:disabled) { background: var(--cp-accent-hover); }
.quiet { border-color: var(--cp-border); padding: 8px 12px; font-size: 13px; }
header { max-width: 1440px; margin: auto; padding: 24px 48px; display: flex; align-items: center; justify-content: space-between; gap: 16px; }
.brand { display: flex; gap: 12px; align-items: center; font-size: 28px; font-weight: 800; letter-spacing: -1px; }
.brand svg { width: 24px; height: 30px; color: var(--cp-accent); }
.header-note { color: var(--cp-text-muted); font-size: 13px; margin-right: auto; padding-left: 16px; border-left: 1px solid var(--cp-border); }
.display-controls { display: flex; flex-wrap: wrap; gap: 8px; justify-content: flex-end; }
.shell { display: grid; grid-template-columns: 272px minmax(0, 880px); gap: 64px; max-width: 1440px; margin: 36px auto 64px; padding: 0 48px; align-items: start; }
aside { padding-top: 24px; position: sticky; top: 32px; }
.journey-heading { font-size: 30px; line-height: 1.25; letter-spacing: -.03em; font-weight: 600; margin: 0 0 32px; max-width: 12ch; }
.journey-heading span { color: var(--cp-accent); }
main { min-width: 0; padding: 44px 48px 0; background: var(--cp-bg-elevated); border: 1px solid var(--cp-border); border-radius: 12px; }
.micro { font-size: 12px; font-weight: 600; color: var(--cp-text-muted); }
h1 { font-size: clamp(32px, 3.7vw, 48px); font-weight: 600; line-height: 1.1; letter-spacing: -.035em; margin: 0 0 20px; text-wrap: balance; }
h2 { font-size: 20px; line-height: 1.3; margin: 0 0 12px; }
p { margin: 0 0 20px; color: var(--cp-text-soft); }
.lead { font-size: 16px; max-width: 62ch; margin-bottom: 32px; }
.step-list { list-style: none; padding: 0; margin: 0 0 32px; }
.step-list li { position: relative; display: flex; align-items: center; gap: 16px; padding: 16px 0; color: var(--cp-text-muted); }
.step-list li:not(:last-child)::after { content: ""; position: absolute; left: 17px; top: 52px; bottom: -12px; width: 1px; background: var(--cp-border); transition: background-color 250ms; }
.step-list .visited::after { background: var(--cp-accent); }
.step-list .number { position: relative; z-index: 1; flex-shrink: 0; width: 36px; height: 36px; border: 1px solid var(--cp-border); border-radius: 50%; background: var(--cp-bg); display: grid; place-items: center; font-size: 13px; }
.step-name small { display: block; font-size: 12px; font-weight: 400; color: var(--cp-text-muted); margin-top: 2px; }
.step-list [aria-current="step"] { color: var(--cp-text); font-weight: 650; }
.step-list [aria-current="step"] .number { background: var(--cp-accent); color: var(--cp-accent-fg); border-color: var(--cp-accent); }
.step-list .visited .number { border-color: var(--cp-accent); color: var(--cp-accent); }
.target-pill { border-top: 1px solid var(--cp-border); padding: 24px 0 0; overflow-wrap: anywhere; }
.target-pill strong { display: block; margin-top: 8px; font-size: 13px; font-family: Consolas, monospace; font-weight: 500; }
.workspace-summary { display: flex; align-items: center; gap: 8px; font-size: 12px; color: var(--cp-text-soft); margin-top: 12px; }
.workspace-summary svg { width: 16px; height: 16px; color: var(--cp-accent); }
.footnote { font-size: 12px; color: var(--cp-text-muted); margin-top: 16px; }
.card { border-block: 1px solid var(--cp-border); padding: 20px 0; margin: 24px 0; }
.choices { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin: 28px 0; }
.choice { position: relative; display: flex; flex-direction: column; gap: 24px; border: 1px solid var(--cp-border); border-radius: 8px; padding: 24px; cursor: pointer; background: var(--cp-bg-elevated); transition: border-color 150ms, background-color 150ms; }
.choice:hover { border-color: var(--cp-border-strong); }
.choice:has(input:checked) { border-color: var(--cp-accent); background: var(--cp-accent-soft); }
.choice:focus-within { outline: 2px solid var(--cp-accent); outline-offset: 4px; }
.choice input { position: absolute; top: 24px; right: 24px; }
.choice-icon { width: 44px; height: 44px; color: var(--cp-text-muted); }
.choice:has(input:checked) .choice-icon { color: var(--cp-accent); }
.choice strong { display: block; font-size: 18px; font-weight: 600; margin-bottom: 8px; }
.choice small { color: var(--cp-text-soft); display: block; font-size: 13px; line-height: 1.6; }
input[type="radio"],input[type="checkbox"] { accent-color: var(--cp-accent); flex-shrink: 0; width: 18px; height: 18px; margin: 4px 0 0; }
.field { display: block; margin: 20px 0; font-weight: 600; }
.field input,.field select { display: block; width: 100%; min-width: 0; padding: 12px 14px; margin-top: 8px; border: 1px solid var(--cp-border-strong); border-radius: 6px; background: var(--cp-surface); color: var(--cp-text); transition: border-color 150ms; }
.field input:hover:not(:disabled),.field select:hover:not(:disabled) { border-color: var(--cp-accent); }
.field small { display: block; font-weight: 400; font-size: 12px; color: var(--cp-text-muted); margin-top: 8px; }
.check-label { display: flex; align-items: flex-start; gap: 12px; margin: 20px 0; cursor: pointer; }
.check-label small { display: block; color: var(--cp-text-muted); margin-top: 4px; }
.bridge-badge { color: var(--cp-bridge); background: var(--cp-bridge-soft); padding: 4px 8px; border-radius: 6px; font-size: 11px; margin-left: 8px; }
.bridge-option { padding: 20px 0; border-block: 1px solid var(--cp-border); margin: 28px 0 16px; }
.bridge-option input { margin-top: 5px; }
.actions { position: sticky; bottom: 0; z-index: 2; display: flex; align-items: center; gap: 12px; justify-content: space-between; margin-top: 32px; padding: 20px 0 24px; border-top: 1px solid var(--cp-border); background: var(--cp-bg-elevated); }
.actions button { min-height: 48px; }
.step-caption { font-size: 12px; color: var(--cp-text-muted); }
.actions .right { margin-left: auto; display: flex; gap: 12px; flex-wrap: wrap; }
.notice { padding: 14px 16px; background: var(--cp-surface-soft); border-radius: 6px; margin: 20px 0; color: var(--cp-text-soft); font-size: 13px; }
#scope-note { padding: 0; background: transparent; color: var(--cp-text-muted); }
#error { border-color: var(--cp-danger); overflow-wrap: anywhere; }
#error strong { color: var(--cp-danger); }
#connection { display: inline-flex; align-items: center; gap: 8px; font-size: 12px; color: var(--cp-text-muted); }
#connection::before { content: ""; width: 6px; height: 6px; border-radius: 50%; background: var(--cp-border-strong); }
html[data-connection="connected"] #connection::before { background: var(--cp-success); }
html[data-connection="interrupted"] #connection::before { background: var(--cp-warning); }
dl { display: grid; grid-template-columns: 140px minmax(0,1fr); gap: 12px; margin: 0; }
dt { color: var(--cp-text-muted); } dd { margin: 0; overflow-wrap: anywhere; }
summary { cursor: pointer; font-weight: 600; }
details p { font-size: 13px; margin: 12px 0 0; }
.checks { list-style: none; margin: 16px 0; padding: 0; }
.checks li { display: grid; grid-template-columns: 20px minmax(0,1fr); gap: 10px; padding: 12px 0; border-bottom: 1px solid var(--cp-border); }
.checks .icon { color: var(--cp-text-muted); }
.checks [data-status="passed"] .icon { color: var(--cp-success); }
.checks [data-status="failed"] .icon { color: var(--cp-danger); }
.checks [data-status="blocked"] .icon { color: var(--cp-warning); }
.checks [data-status="running"] .icon { color: var(--cp-accent); }
.checks small { display: block; color: var(--cp-text-muted); margin-top: 4px; overflow-wrap: anywhere; }
.status-word { font-size: 12px; color: var(--cp-text-muted); font-weight: 400; margin-left: 8px; }
.section-tools { margin: 24px 0 8px; padding-top: 28px; border-top: 1px solid var(--cp-border); }
.section-tools p { font-size: 13px; margin-bottom: 16px; }
#prerequisite-checks,#preflight-checks { display: grid; grid-template-columns: repeat(2,minmax(0,1fr)); column-gap: 24px; }
.checks:empty { display: none !important; }
#prerequisite-message { margin: 12px 0 0; color: var(--cp-text-muted); }
.inline-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin: 16px 0; }
.hint { display: block; font-size: 13px; color: var(--cp-text-muted); margin: 8px 0; }
.hint[data-valid="true"] { color: var(--cp-success); }
.hint[data-valid="false"],input[aria-invalid="true"] { color: var(--cp-danger); }
input[aria-invalid="true"] { border-color: var(--cp-danger); }
.field input::placeholder { color: var(--cp-text-muted); opacity: 1; } /* ci:stub-ok: Standard CSS input-hint pseudo-element. */
.device-code { font: 600 24px/1.4 Consolas, monospace; letter-spacing: .08em; padding: 8px 0; user-select: all; }
#copilot-fields { margin-top: 28px; }
#copilot-fields small { display: block; color: var(--cp-text-muted); }
.provider-picker { display: grid; grid-template-columns: repeat(3,minmax(0,1fr)); gap: 12px; margin: 24px 0 16px; }
.provider-choice { position: relative; display: flex; flex-direction: column; justify-content: center; gap: 6px; min-height: 112px; padding: 20px 32px 20px 16px; border: 1px solid var(--cp-border); border-radius: 8px; cursor: pointer; transition: background-color 150ms, border-color 150ms; }
.provider-choice input { position: absolute; top: 12px; right: 12px; width: 14px; height: 14px; }
.provider-choice strong { font-size: 15px; line-height: 1.3; font-weight: 600; }
.provider-choice small { color: var(--cp-text-muted); font-size: 12px; line-height: 1.4; }
.provider-choice:hover { border-color: var(--cp-border-strong); }
.provider-choice:has(input:checked) { border-color: var(--cp-accent); background: var(--cp-accent-soft); }
.provider-choice:focus-within { outline: 2px solid var(--cp-accent); outline-offset: 4px; }
.provider-choice:has(input:disabled) { opacity: .6; cursor: not-allowed; }
details.card { border-radius: 0; border-width: 1px 0 0; padding: 20px 0; background: transparent; }
.account-tools { padding: 16px 0; border-block: 1px solid var(--cp-border); margin: 24px 0; }
.account-tools p { margin-bottom: 12px; }
fieldset { min-width: 0; border: 0; padding: 0; margin: 24px 0 0; }
fieldset legend { font-size: 20px; font-weight: 650; }
fieldset:disabled { opacity: .6; }
code { overflow-wrap: anywhere; }
#install-hint { font-size: 13px; color: var(--cp-text-muted); }
@keyframes check-working { to { transform: rotate(360deg); } }
@keyframes check-confirm { from { transform: scale(.7); opacity: .4; } to { transform: scale(1); opacity: 1; } }
.checks [data-status="running"] .icon { animation: check-working 1.5s linear infinite; align-self: start; text-align: center; }
.checks.stale .icon { animation: none !important; }
.checks [data-changed="true"][data-status="passed"] .icon { animation: check-confirm 280ms cubic-bezier(.16,1,.3,1); }
.step-list .number { transition: background-color .25s cubic-bezier(.16,1,.3,1), border-color .25s; }
#run-message { white-space: pre-line; overflow-wrap: anywhere; }
.name-row { display: flex; gap: 8px; align-items: center; margin-top: 6px; }
.name-row input { margin: 0; flex: 1; }
.name-row button { flex-shrink: 0; min-height: 46px; }
.ports { display: grid; grid-template-columns: repeat(3,minmax(0,1fr)); gap: 16px; }
.ports .field { margin-bottom: 0; }
.ports-summary { color: var(--cp-text-muted); font-weight: 400; font-size: 13px; margin-left: 8px; }
.run-status { position: relative; padding: 28px; background: var(--cp-bridge-soft); border-radius: 8px; margin: 24px 0; }
.run-status strong { display: block; font-size: 26px; font-weight: 600; line-height: 1.25; color: var(--cp-bridge); margin-bottom: 12px; }
.run-status p { margin: 0 0 12px; font-size: 14px; }
.run-meta { display: flex; flex-wrap: wrap; gap: 8px 20px; font-size: 12px; color: var(--cp-text-muted); }
.run-status[data-state="ready"] { background: var(--cp-accent-soft); }
.run-status[data-state="ready"] strong { color: var(--cp-accent); }
.run-status[data-state="failed"],.run-status[data-state="interrupted"] { background: var(--cp-surface-soft); }
.run-status[data-state="failed"] strong,.run-status[data-state="interrupted"] strong { color: var(--cp-text); }
.run-progress { display: block; width: 100%; height: 4px; appearance: none; border: 0; border-radius: 2px; overflow: hidden; margin: 24px 0 16px; background: var(--cp-border); accent-color: var(--cp-accent); }
.run-progress::-webkit-progress-bar { background: var(--cp-border); }
.run-progress::-webkit-progress-value { background: var(--cp-accent); }
.run-progress::-moz-progress-bar { background: var(--cp-accent); }
#attention { position: sticky; top: 0; z-index: 4; display: flex; align-items: center; justify-content: center; flex-wrap: wrap; gap: 8px 24px; padding: 12px 20px; background: var(--cp-surface); border-bottom: 2px solid var(--cp-accent); }
#attention-message { font-weight: 600; }
#authorization { display: grid; grid-template-columns: 52px minmax(0,1fr); gap: 20px; padding: 24px; background: var(--cp-surface-soft); border: 1px solid var(--cp-border); border-radius: 10px; }
#authorization[data-state="waiting"] { background: var(--cp-accent-soft); border-color: var(--cp-accent); }
.authorization-icon { width: 52px; height: 60px; color: var(--cp-accent); }
#authorization-title { display: block; font-size: 20px; font-weight: 600; line-height: 1.35; margin-bottom: 12px; color: var(--cp-text); }
#authorization-detail { font-size: 14px; margin: 0; }
#authorization[data-state="waiting"] .authorization-icon { animation: authorization-wait 2.8s ease-in-out infinite; }
#authorization[data-state="failed"] .authorization-icon { color: var(--cp-danger); }
@keyframes authorization-wait { 50% { opacity: .45; } }
#install-checks { margin: 28px 0; }
#install-checks li { position: relative; grid-template-columns: 32px minmax(0,1fr); gap: 16px; border: 0; padding: 0 0 28px; }
#install-checks li:not(:last-child)::before { content: ""; position: absolute; top: 32px; left: 15px; bottom: 0; width: 1px; background: var(--cp-border); transition: background-color 250ms; }
#install-checks [data-status="passed"]::before { background: var(--cp-accent); }
#install-checks .icon { display: grid; place-items: center; width: 32px; height: 32px; border: 1px solid var(--cp-border); border-radius: 50%; background: var(--cp-bg-elevated); }
#install-checks [data-status="running"] .icon { border-color: var(--cp-accent); }
#install-checks [data-status="passed"] .icon { background: var(--cp-accent-soft); border-color: var(--cp-accent); color: var(--cp-accent); }
#install-checks li>div { padding-top: 3px; }
#install-checks [data-status="pending"] small,#install-checks [data-status="passed"] small { display: none; }
#recovery { border-top: 1px solid var(--cp-border); padding-top: 24px; margin-top: 24px; }
#recovery p { font-size: 13px; }
#recovery dl { font-size: 13px; gap: 8px 16px; margin-bottom: 20px; }
.command { display: block; padding: 12px; margin: 12px 0; background: var(--cp-surface-soft); white-space: pre-wrap; user-select: all; font-size: 13px; }
footer { padding: 24px 32px; text-align: center; color: var(--cp-text-muted); font-size: 12px; }
@media (min-width: 761px) and (max-width: 1100px) {
 header { padding: 20px 28px; } .header-note { display: none; }
 .shell { grid-template-columns: 208px minmax(0,1fr); padding: 0 28px; gap: 28px; }
 main { padding: 32px 28px 0; } .journey-heading { font-size: 26px; }
 .choice { padding: 20px; } .choice input { top: 20px; right: 20px; }
 .provider-picker { grid-template-columns: repeat(2,minmax(0,1fr)); }
}
@media (max-width: 760px) {
 header { padding: 16px 20px; flex-wrap: wrap; } .header-note { display: none; }
 #connection { order: 3; width: 100%; } .display-controls { margin-left: auto; }
 .shell { grid-template-columns: minmax(0,1fr); margin: 24px auto; padding: 0 20px; gap: 24px; }
 aside { position: static; padding: 0; } aside>.micro,aside>.footnote { display: none; }
 .journey-heading { display: none; }
 main { padding: 28px 20px 0; } h1 { font-size: 34px; }
 .step-list { display: flex; justify-content: space-between; margin: 0 0 20px; gap: 4px; }
 .step-list li { padding: 0; flex: 1; } .step-list li:last-child { flex: 0; }
 .step-list .step-name { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
 .step-list li:not(:last-child)::after { top: 17px; left: 36px; width: calc(100% - 36px); height: 1px; }
 .target-pill { padding: 12px 0 0; } .target-pill strong { display: inline; margin-left: 12px; }
 .workspace-summary { margin-top: 8px; }
 .choices { grid-template-columns: 1fr; gap: 12px; } .card { padding: 20px 0; }
 .choice { flex-direction: row; align-items: flex-start; gap: 16px; padding: 20px 44px 20px 16px; }
 .choice-icon { width: 32px; height: 32px; flex-shrink: 0; }
 .choice input { top: 20px; right: 14px; } .choice strong { font-size: 16px; }
 .provider-picker { grid-template-columns: repeat(2,minmax(0,1fr)); gap: 8px; }
 .provider-choice { padding: 28px 12px 16px; min-height: 112px; }
 #prerequisite-checks,#preflight-checks { grid-template-columns: minmax(0,1fr); }
 .ports { grid-template-columns: 1fr; gap: 0; }
 .ports-summary { display: block; margin: 4px 0 0; }
 .name-row { flex-wrap: wrap; } .name-row input { flex-basis: 160px; }
 dl { grid-template-columns: 1fr; gap: 4px; } dd { margin-bottom: 12px; }
 .actions { flex-wrap: wrap; } .actions .right { flex-wrap: wrap; }
 .step-caption { display: none; }
 #authorization { grid-template-columns: 36px minmax(0,1fr); gap: 16px; }
 .authorization-icon { width: 36px; height: 44px; } .run-status { padding: 20px; }
}
html[data-motion="off"] *,html[data-motion="off"] *::before,html[data-motion="off"] *::after { animation: none !important; transition: none !important; }
html[data-visibility="hidden"] *,html[data-connection="interrupted"] .checks .icon { animation-play-state: paused !important; }
html[data-authorization="waiting"] #preflight-checks .icon,html[data-authorization="waiting"] #install-checks .icon { animation-play-state: paused !important; }
@media (prefers-reduced-motion: reduce) { *,*::before,*::after { scroll-behavior: auto; animation: none !important; transition: none !important; } }
`;
