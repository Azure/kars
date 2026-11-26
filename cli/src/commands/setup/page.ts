// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { setupPasswordIssue } from "./password.js";
import { setupPageStyles } from "./page-styles.js";

/** No credentials or cluster values are interpolated into the document. */
export function renderSetupPage(): string {
  return String.raw`<!doctype html>
<html lang="en" data-brand="kars">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>Set up your workspace · kars</title>
<style>${setupPageStyles}</style>
</head>
<body>
<div id="attention" hidden><span id="attention-message" role="alert"></span><button type="button" class="quiet" id="show-authorization">Show instructions</button></div>
<header>
 <div class="brand"><svg viewBox="0 0 32 36" aria-hidden="true"><path d="M2 3v30h6V21l13 12h9L13 17 29 3h-9L8 14V3Z" fill="currentColor"/></svg>kars</div>
 <span class="header-note">Workspace setup</span>
 <span id="connection" role="status">Connecting securely…</span>
 <div class="display-controls"><button class="quiet" id="motion" type="button" aria-pressed="false">Pause motion</button><button class="quiet" id="theme" type="button" aria-label="Switch color theme">Light / dark</button></div>
</header>
<div class="shell">
 <aside aria-label="Setup progress">
  <p class="journey-heading">Your infrastructure.<br><span>Your workspace.</span></p>
  <ol class="step-list">
   <li aria-current="step"><span class="number" aria-hidden="true">1</span><span class="step-name">Choose a home<small>Destination &amp; prerequisites</small></span></li>
   <li><span class="number" aria-hidden="true">2</span><span class="step-name">Connect inference<small>Your provider, your model</small></span></li>
   <li><span class="number" aria-hidden="true">3</span><span class="step-name">Your access<small>A private first sign-in</small></span></li>
   <li><span class="number" aria-hidden="true">4</span><span class="step-name">Review &amp; check<small>You approve the changes</small></span></li>
   <li><span class="number" aria-hidden="true">5</span><span class="step-name">Bring it online<small>Follow each installation stage</small></span></li>
  </ol>
  <div class="target-pill"><span class="micro">Selected destination</span><strong id="target-label">New local Kind cluster</strong></div>
  <div class="workspace-summary"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/></svg><span id="component-summary">Core + Bridge</span></div>
  <p class="footnote">This concierge runs on your computer. Nothing is installed until you review and approve the changes.</p>
 </aside>
 <main id="main">
  <div id="error" class="notice" role="alert" hidden><strong>We need to resolve something.</strong><div id="error-text"></div></div>
  <form id="setup" autocomplete="off">
   <section data-panel="0" aria-labelledby="destination-heading">
    <h1 id="destination-heading" tabindex="-1">Set up your workspace.</h1>
    <p class="lead">Choose where Kars runs. We’ll check your tools, connect a model, and set up your first sign-in.</p>
    <div class="choices">
     <label class="choice"><input type="radio" name="target" value="kind" checked><svg class="choice-icon" viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" aria-hidden="true"><rect x="8" y="8" width="32" height="24" rx="3"/><path d="M8 32 3 39h42l-5-7M19 39h10M19 19l-4 4 4 4m10-8 4 4-4 4"/></svg><span><strong>On this computer</strong><small>A fresh Kind cluster using Podman. Existing clusters stay untouched.</small></span></label>
     <label class="choice"><input type="radio" name="target" value="existing"><svg class="choice-icon" viewBox="0 0 48 48" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" aria-hidden="true"><rect x="8" y="6" width="32" height="14" rx="3"/><rect x="8" y="28" width="32" height="14" rx="3"/><path d="M24 20v8M14 13h2m4 0h2m-8 22h2m4 0h2m10-22h2m-2 22h2"/></svg><span><strong>Existing cluster</strong><small>Select a Kubernetes context, including AKS. Compatibility and permissions are checked first.</small></span></label>
    </div>
    <div class="field" id="cluster-field"><label for="cluster">Cluster name</label><div class="name-row"><input id="cluster" pattern="[a-z0-9][a-z0-9\-]{0,39}" maxlength="40" required spellcheck="false" aria-describedby="cluster-hint"><button type="button" class="quiet" id="generate-cluster">Generate name</button></div><small id="cluster-hint">Generated for this setup. Edit it if you like; existing clusters are never replaced.</small></div>
    <label class="field" id="context-field" hidden>Kubernetes context<select id="context" disabled><option value="">Select a context</option></select><small>All operations stay pinned to your selection. Your current context is not switched.</small></label>
    <label class="check-label bridge-option"><input id="bridge" type="checkbox" checked><span><strong>Include Bridge</strong><span class="bridge-badge">Visual workspace</span><small>Create teams, follow work, and review results in your browser. Leave unchecked for Core only.</small></span></label>
    <div class="notice" id="scope-note" role="status"></div>
    <details class="card" id="local-ports"><summary>Local ports <span class="ports-summary" id="ports-summary"></span></summary>
     <p>Only this computer can connect. Choose unused ports from 1024 to 65535; privileged port 443 is not supported by macOS Podman.</p>
     <div class="ports">
      <label class="field">Sign-in HTTPS<input id="https-port" type="number" min="1024" max="65535" step="1" value="8443" required></label>
      <label class="field" id="bridge-port-field">Bridge<input id="bridge-port" type="number" min="1024" max="65535" step="1" value="3000" required></label>
      <label class="field">Headlamp<input id="headlamp-port" type="number" min="1024" max="65535" step="1" value="4466" required></label>
     </div>
    </details>
    <div id="prerequisites-section" class="section-tools">
     <h2>First, check your computer.</h2>
     <p>Check your selected ports, tools and running Podman service before entering credentials. Busy ports block setup; no running service is stopped.</p>
     <button type="button" class="primary" id="prerequisites">Check prerequisites &amp; ports</button>
     <ul id="prerequisite-checks" class="checks" aria-label="Prerequisite results"></ul>
     <p id="prerequisite-message" role="status">Run the check to continue.</p>
    </div>
    <div class="actions"><span class="step-caption">Next · Connect inference</span><div class="right"><button type="button" class="primary" data-next>Continue →</button></div></div>
   </section>
   <section data-panel="1" hidden aria-labelledby="provider-heading">
    <h1 id="provider-heading" tabindex="-1">Connect inference.</h1>
    <p class="lead">SRE needs a working model. If you include Bridge, its orchestrator uses the same defaults. Preflight sends one small, billable test request after local prerequisites pass.</p>
    <div id="provider-fields">
     <label hidden>Provider<select id="provider"><option value="github-copilot">GitHub Copilot</option><option value="openai">OpenAI</option><option value="anthropic">Anthropic</option><option value="foundry">Azure AI Foundry</option><option value="azure-openai">Azure OpenAI</option><option value="openai-compatible">Self-hosted / OpenAI-compatible</option></select></label>
     <div class="provider-picker" role="group" aria-label="Inference provider">
      <label class="provider-choice"><input type="radio" name="inference" value="github-copilot" checked><strong>GitHub Copilot</strong><small>Sign in with GitHub</small></label>
      <label class="provider-choice"><input type="radio" name="inference" value="openai"><strong>OpenAI</strong><small>Project API key</small></label>
      <label class="provider-choice"><input type="radio" name="inference" value="anthropic"><strong>Anthropic</strong><small>Compatible API</small></label>
      <label class="provider-choice"><input type="radio" name="inference" value="foundry"><strong>Azure AI Foundry</strong><small>Existing deployment</small></label>
      <label class="provider-choice"><input type="radio" name="inference" value="azure-openai"><strong>Azure OpenAI</strong><small>Existing deployment</small></label>
      <label class="provider-choice"><input type="radio" name="inference" value="openai-compatible"><strong>Self-hosted</strong><small>OpenAI-compatible</small></label>
     </div>
     <p class="hint">No Azure account is required for non-Azure providers.</p>
     <div id="provider-note" class="notice" role="status"></div>
     <label class="field" id="endpoint-field">API base URL<input id="endpoint" type="url" required spellcheck="false"><small>Use the API base URL, not a chat/completions URL. Keys never belong in the URL.</small></label>
     <label class="field" id="model-field"><span id="model-label">Model</span><input id="model" placeholder="Model ID available to your account" maxlength="256" pattern="[A-Za-z0-9][A-Za-z0-9._:\/\-]*" required spellcheck="false"><small>The provider and model become Core defaults for SRE, and for Bridge when installed.</small></label>
     <label class="field" id="api-key-field">API key<input id="api-key" type="password" autocomplete="new-password" required spellcheck="false"><small>Sent only to this local installer and the configured provider/in-cluster credential path. Never saved in browser storage.</small></label>
     <div id="copilot-fields" hidden>
      <p id="copilot-status" role="status">Sign in with a GitHub account that has Copilot access.</p>
      <div id="copilot-code-row" class="notice" hidden>
       <span class="hint">Copy this one-time code, then enter it on GitHub.</span>
       <div class="inline-actions"><strong id="copilot-code" class="device-code"></strong><button type="button" class="quiet" id="copy-code">Copy code</button></div>
       <a class="button" href="https://github.com/login/device" target="_blank" rel="noopener noreferrer">Open GitHub ↗</a>
       <span id="code-feedback" class="hint" role="status"></span><span id="code-expiry" class="hint"></span>
      </div>
      <div class="inline-actions"><button type="button" id="copilot-login">Sign in with GitHub</button><button type="button" id="copilot-cancel" hidden>Cancel sign-in</button></div>
      <small>Your OAuth token stays in the installer process. Preflight verifies Copilot access and a real model reply.</small>
      <label class="field">Copilot model<select id="copilot-model" required disabled><option value="">Sign in to load models</option></select><small>Live chat models from your account. Your selection becomes Core defaults for SRE, and for Bridge when installed. A listed model still needs to pass the real response check.</small></label>
      <p id="models-message" class="hint" role="status"></p><button type="button" class="quiet" id="refresh-models" hidden>Reload models</button>
     </div>
    </div>
    <details class="card"><summary>What will the connection check do?</summary><p>Verification may make a small billable inference request. Listing available models alone does not prove the workspace can execute. Provider limits still apply; actual token usage remains accounted for.</p></details>
    <div class="actions"><button type="button" data-back>← Back</button><div class="right"><button type="button" class="primary" data-next>Continue →</button></div></div>
   </section>
   <section data-panel="2" hidden aria-labelledby="access-heading">
    <h1 id="access-heading" tabindex="-1">Make it your workspace.</h1>
    <p class="lead" id="access-description">Create the first administrator for your local workspace. This is a new account, not your Microsoft or provider password.</p>
    <div id="local-account">
     <div class="account-tools"><p>Just testing? Generate a unique local account, then save its details somewhere private.</p><div class="inline-actions"><button type="button" id="generate-account">Generate test account</button><button type="button" class="quiet" id="copy-account">Copy account details</button></div><span id="account-feedback" class="hint" role="status"></span><small class="hint">Copying includes the password. Your system clipboard may sync to other devices.</small></div>
     <label class="field"><span id="account-email-label">Administrator email</span><input id="admin-email" type="email" autocomplete="username" maxlength="254" required></label>
     <label class="field">New password<input id="admin-password" type="password" autocomplete="new-password" minlength="12" aria-describedby="password-hint" required><small id="password-hint" class="hint" role="status">12+ characters, uppercase, lowercase, a number and a symbol; at most 72 UTF-8 bytes.</small></label>
     <label class="field">Confirm password<input id="admin-confirm" type="password" autocomplete="new-password" minlength="12" aria-describedby="confirm-hint" required><small id="confirm-hint" class="hint" role="status">Enter the same password again.</small></label>
     <label class="check-label"><input id="show-password" type="checkbox"><span>Show passwords</span></label>
     <div class="notice">Local sign-in uses the packaged identity provider and verified HTTPS. No shared default account or development login.</div>
    </div>
    <div id="external-account" class="notice" hidden>Existing clusters need their real identity configuration and authorized workspace enrollment. Preflight will report missing requirements; we won’t substitute a local account or weaken authentication.</div>
    <div id="core-account" class="notice" hidden>Core-only keeps Headlamp with secure sign-in and read-only dashboard access. No Bridge administrator is created. Configuration changes remain with your Kubernetes operator identity.</div>
    <div class="actions"><button type="button" data-back>← Back</button><div class="right"><button type="button" class="primary" data-next>Review setup →</button></div></div>
   </section>
   <section data-panel="3" hidden aria-labelledby="review-heading">
    <h1 id="review-heading" tabindex="-1">Review your setup.</h1>
    <p class="lead">We’ll check the target, tools, release assets, and configuration. Review the results before installing.</p>
    <div class="card"><dl id="review"></dl></div>
    <details class="card"><summary>Boundaries and ownership</summary><p>Setup uses the maintained release and Helm paths. Existing resources are not silently adopted, overwritten, or deleted. Failed checks stay visible. Readiness means verified checks—not just running pods. A failed installation may need targeted recovery; setup never blindly replays consumed installation identities.</p></details>
    <button type="button" id="preflight">Check this setup</button>
    <ul id="preflight-checks" class="checks" aria-label="Preflight results"></ul>
    <p id="preflight-message" role="status"></p>
    <details class="card" id="trust-explanation"><summary>Why does local setup need hostname and certificate permission?</summary><p>Your local sign-in needs a trusted HTTPS address. Setup adds one marked <code>/etc/hosts</code> entry pointing a unique <code>&lt;cluster&gt;-&lt;attempt&gt;.kars.test</code> hostname to <code>127.0.0.1</code>. It creates a one-year CA: macOS trusts it only for that hostname in your login keychain; supported Linux hosts use the system CA store. The signing key is destroyed after issuance. There is no TLS bypass.</p><p>These owned changes remain until you run the maintained <code>kars dev down --target local-k8s --cluster-name &lt;cluster&gt;</code> teardown. Closing this tab does not remove them. Independent browser trust stores may still ask for approval.</p><p>If host access needs authorization, setup prompts in its launching terminal and waits here. Enter your OS password only in that terminal or the macOS certificate dialog, never in this browser. Approval continues the same session automatically; no retry or new sign-in is needed. Preflight does not change host trust or install anything. If you do not want local trust changes, use the reviewed CLI/Helm path with your own OIDC and TLS; do not disable certificate verification.</p></details>
    <fieldset id="final-review" disabled><legend>Confirm this installation</legend>
     <p class="hint">Available only after every check passes. Editing setup inputs requires a fresh check.</p>
     <label class="check-label" id="trust-field"><input id="trust" type="checkbox"><span>I approve the local hostname and certificate-trust changes described above.</span></label>
     <div class="inline-actions"><strong id="approve-target"></strong><button type="button" class="quiet" id="copy-destination">Copy destination</button><span id="destination-feedback" class="hint" role="status"></span></div>
     <label class="check-label"><input id="approve" type="checkbox"><span>I approve installing the selected components into the destination above.<small>Downloading images and allocating cluster resources can take time and incur cloud/provider costs.</small></span></label>
     <label class="field">Confirm destination<input id="confirm-target" autocomplete="off" spellcheck="false" aria-describedby="install-hint"><small>Type the exact destination shown above to unlock installation.</small></label>
    </fieldset>
    <p id="install-hint" role="status">Run and pass the setup check first.</p>
    <div class="actions"><button type="button" data-back>← Back</button><div class="right"><button type="submit" id="install" class="primary" disabled>Install workspace →</button></div></div>
   </section>
   <section data-panel="4" hidden aria-labelledby="run-heading">
    <h1 id="run-heading" tabindex="-1">Bringing it online.</h1>
    <p class="lead" id="run-message" role="status">Waiting for the installer to report progress.</p>
    <div class="notice" id="authorization" role="region" tabindex="-1" aria-labelledby="authorization-title" aria-describedby="authorization-detail" hidden><svg class="authorization-icon" viewBox="0 0 52 60" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M26 3 47 11v17c0 13-10 22-21 29C15 50 5 41 5 28V11Z"/><rect x="17" y="26" width="18" height="15" rx="3"/><path d="M20 26v-5a6 6 0 0 1 12 0v5m-6 6v3"/></svg><div><strong id="authorization-title"></strong><p id="authorization-detail"></p></div></div>
    <div class="run-status" id="run-status"><strong id="run-current" role="status">Requesting installation…</strong><p id="run-detail">Checking the approved plan before making changes.</p><progress id="run-progress" class="run-progress" value="0" max="1" aria-label="Completed installation stages"></progress><div class="run-meta"><span id="run-count">Waiting for the installer</span><span id="run-elapsed"></span></div></div>
    <ul id="install-checks" class="checks" aria-label="Installation progress"></ul>
    <details id="live-resources" hidden open><summary>Live workloads &amp; Helm hooks</summary><p id="observation-message" role="status"></p><small id="observation-time"></small><ul id="resource-checks" class="checks" aria-label="Observed Kubernetes resources"></ul></details>
    <section id="recovery" hidden aria-labelledby="recovery-heading"><h2 id="recovery-heading">What was left behind</h2><dl id="recovery-state"></dl><p>Setup never deletes resources automatically after failure. Cleanup below requires your explicit confirmation.</p><div id="owned-cleanup" hidden><label class="check-label"><input id="confirm-cleanup" type="checkbox"><span id="cleanup-consent"></span></label><div class="inline-actions"><button type="button" id="run-cleanup" disabled>Remove this failed installation</button><span id="cleanup-status" role="status"></span></div></div><div id="cleanup" hidden><details><summary>Terminal cleanup instead</summary><p>This command uses this installer's exact CLI, rechecks ownership and removes only the recorded node and host-trust changes. Close the installer, then run it from the working directory shown above. Private receipts remain.</p><code id="cleanup-command" class="command"></code></details></div><p id="recovery-guidance"></p></section>
    <div class="notice" id="run-note">Keep the launching terminal open. Closing this tab does not uninstall anything. If setup stops, the recorded failure and existing resources are preserved.</div>
    <div class="actions"><button type="button" id="return-review" hidden>← Review the issue</button><div class="right"><a id="handoff" class="button primary" hidden>Open Bridge →</a><button type="button" id="shutdown" hidden>Close installer</button></div></div>
   </section>
  </form>
  <noscript><div class="notice">This local setup interface requires JavaScript. You can still use the terminal CLI or documented Helm installation.</div></noscript>
 </main>
</div>
<footer>kars setup · Local concierge · Credentials are never stored in browser storage</footer>
<script>
(() => {
  const param = new URLSearchParams(window.location.search).get("scoutTheme");
  const theme = param || (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  document.documentElement.setAttribute("data-theme", theme);
})();
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  let csrf = '', screen = 0, authenticated = false, busy = false, checkedDraft = '', pendingPreflight = '', timer, checking = '', pollGeneration = 0;
  let state = { contexts: [], checks: [], steps: [] };
  const passwordIssue = ${setupPasswordIssue.toString()};
  const panels = Array.from(document.querySelectorAll('[data-panel]'));
  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  let motionPaused = false;
  function syncMotion() {
    const off = motionPaused || reducedMotion.matches;
    document.documentElement.dataset.motion = off ? 'off' : 'on';
    document.documentElement.dataset.visibility = document.hidden ? 'hidden' : 'visible';
    $('motion').disabled = reducedMotion.matches;
    $('motion').textContent = reducedMotion.matches ? 'Motion reduced' : motionPaused ? 'Resume motion' : 'Pause motion';
    $('motion').setAttribute('aria-pressed', String(off));
    if (off || document.hidden) document.getAnimations().forEach(animation => {
      if (off || !(animation instanceof CSSAnimation)) animation.cancel();
    });
  }
  function animateChange(element, frames, duration) {
    if (motionPaused || reducedMotion.matches || document.hidden || !element.getClientRects().length) return;
    element.getAnimations().forEach(animation => animation.cancel());
    element.animate(frames, { duration, easing: 'cubic-bezier(.16,1,.3,1)' });
  }
  reducedMotion.addEventListener('change', syncMotion);
  document.addEventListener('visibilitychange', syncMotion);
  $('motion').addEventListener('click', () => { motionPaused = !motionPaused; syncMotion(); });
  syncMotion();
  const target = () => document.querySelector('input[name="target"]:checked').value;
  const wantsLocalAccount = () => target() === 'kind';
  const supported = () => target() === 'kind';
  const providerEndpoints = { openai: 'https://api.openai.com/v1', anthropic: 'https://api.anthropic.com/v1', 'github-copilot': 'https://api.githubcopilot.com' };
  const providerNotes = {
    openai: 'Connect directly to the OpenAI API with a project API key and an available model.',
    anthropic: 'Uses Anthropic’s official OpenAI-compatible Chat Completions API for the shared SRE/Bridge path. Native Messages-only features are not enabled by this setup. Anthropic recommends its native API for production features; this compatibility route is not production qualification.',
    'github-copilot': 'Use your existing Copilot access. Sign-in does not guarantee every model is available; preflight checks the model you choose.',
    foundry: 'Connect an existing public Azure AI Foundry resource and model deployment.',
    'azure-openai': 'Connect an existing public Azure OpenAI resource and model deployment.',
    'openai-compatible': 'Connect an existing HTTPS model server reachable by the cluster. This does not deploy a model or GPU. In-cluster KAITO, vLLM and Ollama services need service discovery and precise egress configuration, which this fresh-Kind installer does not configure yet. Laptop localhost is not a sandbox endpoint.'
  };
  const error = (message) => { $('error-text').textContent = message; $('error').hidden = !message; };
  const destination = () => target() === 'kind' ? 'kind-' + $('cluster').value : $('context').value || 'No context selected';
  const selectedModel = () => $('provider').value === 'github-copilot' ? $('copilot-model').value : $('model').value.trim();
  const ports = () => ({ https: Number($('https-port').value), bridge: $('bridge').checked ? Number($('bridge-port').value) : 3000, headlamp: Number($('headlamp-port').value) });
  function generateCluster() {
    $('cluster').value = 'kars-' + Array.from(crypto.getRandomValues(new Uint8Array(4)), byte => byte.toString(16).padStart(2,'0')).join('');
  }
  function draft() {
    return {
      target: target(), clusterName: $('cluster').value.trim(), context: $('context').value,
      bridge: $('bridge').checked, ports: ports(),
      provider: { kind: $('provider').value, endpoint: $('endpoint').value.trim(), model: selectedModel(), ...($('provider').value === 'github-copilot' ? {} : { apiKey: $('api-key').value }) },
      admin: wantsLocalAccount() ? { email: $('admin-email').value.trim(), password: $('admin-password').value } : undefined,
      consent: { target: $('approve').checked, hostTrust: $('trust').checked }
    };
  }
  // The in-memory equality binding is not an authority check; the server revalidates the approved plan.
  function binding() { const d = draft(); delete d.consent; return JSON.stringify([d, wantsLocalAccount() ? $('admin-confirm').value : '']); }
  function invalidate() {
    checkedDraft = ''; pendingPreflight = ''; $('approve').checked = false; $('trust').checked = false; $('confirm-target').value = '';
  }
  function prerequisiteSelectionMatches() {
    const selection = state.prerequisiteSelection, current = ports();
    return selection && selection.bridge === $('bridge').checked
      && selection.ports.https === current.https && selection.ports.headlamp === current.headlamp
      && (!selection.bridge || selection.ports.bridge === current.bridge);
  }
  function prerequisitesReady() { return state.prerequisitesReady && prerequisiteSelectionMatches(); }
  function passwordFeedback() {
    const password = $('admin-password').value, confirm = $('admin-confirm').value, issue = passwordIssue(password);
    $('admin-password').setCustomValidity(issue);
    $('admin-confirm').setCustomValidity(confirm === password ? '' : 'The passwords do not match.');
    $('password-hint').textContent = password ? issue || 'Password meets the requirements.' : '12+ characters, uppercase, lowercase, a number and a symbol; at most 72 UTF-8 bytes.';
    $('confirm-hint').textContent = confirm ? (confirm === password ? 'Passwords match.' : 'Passwords do not match yet.') : 'Enter the same password again.';
    for (const [input, hint, entered, valid] of [['admin-password','password-hint',password,!issue],['admin-confirm','confirm-hint',confirm,confirm === password]]) {
      $(hint).dataset.valid = entered ? String(valid) : '';
      if (entered) $(input).setAttribute('aria-invalid', String(!valid)); else $(input).removeAttribute('aria-invalid');
    }
  }
  async function copy(value, feedback, fallback) {
    try { await navigator.clipboard.writeText(value); $(feedback).textContent = 'Copied to clipboard.'; }
    catch { $(feedback).textContent = fallback; }
  }
  function renderModels(auth) {
    const select = $('copilot-model'), selected = select.value, models = auth.modelsPhase === 'ready' ? auth.models || [] : [];
    const labels = models.map(model => [model.id, model.name + (model.name === model.id ? '' : ' · ' + model.id)]);
    const signature = JSON.stringify([auth.modelsPhase, labels]);
    if (select.dataset.catalog !== signature) {
      select.dataset.catalog = signature;
      select.replaceChildren(new Option(auth.modelsPhase === 'loading' ? 'Loading your models…' : models.length ? 'Choose a model' : 'Sign in or reload models', ''));
      for (const [id, label] of labels) select.append(new Option(label, id));
      if (models.some(model => model.id === selected)) select.value = selected;
      if (selected !== select.value) invalidate();
    }
    select.disabled = busy || !supported() || $('provider').value !== 'github-copilot' || auth.modelsPhase !== 'ready';
    $('models-message').textContent = auth.modelsMessage || (auth.modelsPhase === 'loading' ? 'Checking your account’s enabled chat models…' : models.length ? models.length + ' chat models available. Select one to verify its response.' : '');
    $('refresh-models').hidden = auth.phase !== 'connected';
    $('refresh-models').disabled = busy || auth.modelsPhase === 'loading';
  }
  function configureFields() {
    const local = target() === 'kind', account = wantsLocalAccount();
    const provider = $('provider').value, copilot = provider === 'github-copilot', azure = provider === 'foundry' || provider === 'azure-openai';
    $('endpoint').placeholder = provider === 'foundry' ? 'https://your-resource.services.ai.azure.com' : provider === 'azure-openai' ? 'https://your-resource.openai.azure.com' : 'https://models.example.com/v1';
    $('endpoint').readOnly = Boolean(providerEndpoints[provider]);
    $('endpoint-field').hidden = Boolean(providerEndpoints[provider]);
    $('provider-note').textContent = providerNotes[provider] + (local && !$('bridge').checked && !azure ? ' Core-only uses a dedicated installer identity and short-lived credentials to enroll this provider. Bridge is not required or added.' : '');
    $('model-label').textContent = azure ? 'Model deployment' : 'Model';
    $('model').placeholder = azure ? 'Your deployment name' : 'Model ID available to your account';
    $('model-field').hidden = copilot;
    $('api-key-field').hidden = copilot;
    $('copilot-fields').hidden = !copilot;
    $('cluster-field').hidden = !local; $('cluster').disabled = !local || busy;
    $('generate-cluster').disabled = !local || busy;
    $('local-ports').hidden = !local;
    $('bridge-port-field').hidden = !$('bridge').checked;
    const activePorts = ['https-port', ...($('bridge').checked ? ['bridge-port'] : []), 'headlamp-port'];
    for (const id of ['https-port','bridge-port','headlamp-port']) {
      const input = $(id); input.disabled = !local || busy || !activePorts.includes(id);
      input.setCustomValidity(activePorts.includes(id) && activePorts.some(other => other !== id && $(other).valueAsNumber === input.valueAsNumber) ? 'Each service needs a different local port.' : '');
    }
    $('ports-summary').textContent = 'HTTPS ' + ports().https + ($('bridge').checked ? ' · Bridge ' + ports().bridge : '') + ' · Headlamp ' + ports().headlamp;
    $('context-field').hidden = local; $('context').disabled = local || busy; $('context').required = !local;
    document.querySelectorAll('input[name="target"],#bridge').forEach(input => { input.disabled = busy; });
    $('prerequisites-section').hidden = !local;
    $('prerequisites').disabled = !authenticated || busy;
    $('prerequisites').classList.toggle('primary', !prerequisitesReady());
    $('prerequisites').textContent = checking === 'prerequisites' || state.phase === 'prerequisites' ? 'Checking prerequisites & ports…' : prerequisitesReady() ? 'Check again' : 'Check prerequisites & ports';
    $('local-account').hidden = !account;
    $('external-account').hidden = local || !$('bridge').checked;
    $('core-account').hidden = $('bridge').checked;
    $('access-description').textContent = account ? ($('bridge').checked ? 'Create the first administrator for your local workspace. This is a new account, not your Microsoft or provider password.' : 'Create your local Headlamp sign-in. This new account has read-only dashboard access, not cluster-administrator privileges.') : 'Reuse your cluster’s identity and access. Setup must verify it before handing over.';
    $('account-email-label').textContent = $('bridge').checked ? 'Administrator email' : 'Headlamp account email';
    ['admin-email','admin-password','admin-confirm','generate-account','copy-account','show-password'].forEach((id) => { $(id).disabled = !account || busy; });
    $('trust-field').hidden = !account;
    $('trust-explanation').hidden = !account;
    ['provider','endpoint','model','api-key'].forEach((id) => { $(id).disabled = !supported() || busy; });
    document.querySelectorAll('input[name="inference"]').forEach(input => {
      input.checked = input.value === provider;
      input.disabled = !supported() || busy;
    });
    $('api-key').disabled = !supported() || busy || copilot;
    $('model').disabled = !supported() || busy || copilot;
    const auth = state.copilot || { phase: 'idle' }, signingIn = ['starting','pending'].includes(auth.phase);
    $('copilot-login').disabled = !authenticated || !supported() || busy || signingIn;
    $('copilot-login').textContent = auth.phase === 'connected' ? 'Use a different GitHub account' : 'Sign in with GitHub';
    $('copilot-cancel').hidden = !signingIn; $('copilot-cancel').disabled = busy;
    $('copilot-code-row').hidden = auth.phase !== 'pending';
    $('copilot-code').textContent = auth.userCode || '';
    $('copy-code').disabled = !auth.userCode || busy || auth.phase !== 'pending' || !auth.expiresAt || Date.now() >= auth.expiresAt;
    $('code-expiry').textContent = auth.expiresAt ? 'Expires at ' + new Date(auth.expiresAt).toLocaleTimeString() + '. Never share this sign-in code.' : '';
    $('copilot-status').textContent = auth.message || ({ idle: 'Sign in with a GitHub account that has Copilot access.', starting: 'Requesting a private sign-in code…', pending: 'Complete sign-in on GitHub. This code expires automatically.', connected: 'Signed in. Choose a model; preflight verifies your access.', failed: 'Sign-in failed. Please retry.' })[auth.phase];
    renderModels(auth);
    $('target-label').textContent = state.installation ? 'kind-' + state.installation.clusterName : state.recovery?.clusterName ? 'kind-' + state.recovery.clusterName : destination(); $('approve-target').textContent = destination();
    $('component-summary').textContent = state.recovery && !state.installation ? 'Retained failed attempt' : (state.installation ? state.installation.bridge : $('bridge').checked) ? 'Core + Bridge' : 'Core only';
    $('scope-note').textContent = supported() ? ($('bridge').checked ? 'Install a fresh local Kind cluster on Podman with Core + Bridge. Packaged release assets and authorized host trust are required.' : 'Install Core with secure Headlamp and your chosen inference provider, without Bridge. Packaged release assets and authorized host trust are required.') : 'This destination is inspection-only in this version. Use the terminal CLI or reviewed Helm path for existing AKS installations; the concierge will not modify or adopt them.';
    updateInstall();
  }
  function show(index) {
    const previous = screen;
    panels.forEach(panel => panel.getAnimations().forEach(animation => animation.cancel()));
    screen = index; panels.forEach((panel, i) => { panel.hidden = i !== index; });
    document.querySelectorAll('.step-list li').forEach((li, i) => {
      if (i === index) li.setAttribute('aria-current','step'); else li.removeAttribute('aria-current');
      li.classList.toggle('visited', i < index);
      li.querySelector('.number').textContent = i < index ? '✓' : String(i + 1);
    });
    configureFields(); if (index === 3) renderReview();
    panels[index].querySelector('h1').focus();
    if (previous !== index) animateChange(panels[index], [{ transform: 'translateX(' + (index > previous ? 18 : -18) + 'px)', opacity: .4 }, { transform: 'translateX(0)', opacity: 1 }], 280);
  }
  function valid(index) {
    configureFields();
    if (index === 0 && supported() && !prerequisitesReady()) {
      error('Check prerequisites and the selected ports before continuing.'); return false;
    }
    if (index === 1 && supported() && $('provider').value === 'github-copilot'
      && (state.copilot?.phase !== 'connected' || state.copilot.modelsPhase !== 'ready' || !selectedModel())) {
      error('Sign in with GitHub Copilot and choose a model from your account before continuing.'); return false;
    }
    if (index === 2 && wantsLocalAccount()) {
      passwordFeedback();
    }
    for (const input of panels[index].querySelectorAll('input,select')) {
      if (!input.disabled && !input.checkValidity()) { show(index); if (input.closest('#local-ports')) $('local-ports').open = true; input.reportValidity(); return false; }
    }
    return true;
  }
  function renderReview() {
    const entries = [ ['Destination', destination()], ['Components', $('bridge').checked ? 'Core + Bridge' : 'Core only'], ['Inference', supported() ? $('provider').selectedOptions[0].textContent + ' · ' + selectedModel() : 'Existing configuration remains untouched'], ['Sign-in', wantsLocalAccount() ? $('admin-email').value : $('bridge').checked ? 'Existing cluster identity' : 'Kubernetes identity'] ];
    if (supported()) entries.push(['Local access', 'HTTPS :' + ports().https + ($('bridge').checked ? ' · Bridge :' + ports().bridge : '') + ' · Headlamp :' + ports().headlamp]);
    $('review').replaceChildren();
    for (const [label, value] of entries) { const dt = document.createElement('dt'), dd = document.createElement('dd'); dt.textContent = label; dd.textContent = value; $('review').append(dt, dd); }
  }
  function renderChecks(id, checks) {
    const list = $(id), previous = new Map(Array.from(list.children, li => [li.dataset.key, li]));
    for (const [index, check] of (checks || []).entries()) {
      const status = ['pending','running','passed','failed','blocked'].includes(check.status) ? check.status : 'pending';
      const key = check.id, li = previous.get(key) || document.createElement('li');
      previous.delete(key);
      if (!li.children.length) {
        const icon = document.createElement('span'), body = document.createElement('div'), title = document.createElement('strong'), word = document.createElement('span'), detail = document.createElement('small');
        word.className = 'status-word'; body.append(title, word, detail); li.append(icon, body);
      }
      const icon = li.firstElementChild, body = li.lastElementChild, title = body.querySelector('strong'), word = body.querySelector('.status-word'), detail = body.querySelector('small');
      if (li.dataset.status !== status) li.dataset.changed = String(Boolean(li.dataset.status));
      li.dataset.key = key;
      li.dataset.status = status; icon.className = 'icon'; icon.setAttribute('aria-hidden','true');
      const symbol = ({passed:'✓',failed:'×',blocked:'!',running:'↻',pending:'○'})[status];
      if (icon.textContent !== symbol) icon.textContent = symbol;
      if (title.textContent !== (check.label || check.id || 'Check')) title.textContent = check.label || check.id || 'Check';
      if (word.textContent !== status) word.textContent = status;
      if (detail.textContent !== (check.detail || '')) detail.textContent = check.detail || '';
      detail.hidden = !check.detail;
      if (list.children[index] !== li) list.insertBefore(li, list.children[index] || null);
    }
    previous.forEach(li => li.remove());
  }
  function updateInstall() {
    const passed = authenticated && !busy && state.phase === 'review' && checkedDraft && checkedDraft === binding();
    $('final-review').disabled = !passed;
    $('install').disabled = !passed || !$('approve').checked || (wantsLocalAccount() && !$('trust').checked) || $('confirm-target').value !== destination();
    $('install-hint').textContent = !passed ? 'Run and pass the setup check first.' : $('install').disabled ? 'Review the permissions and type ' + destination() + ' to unlock installation.' : 'Checks and confirmation match this destination. Ready to install.';
    $('preflight').disabled = !authenticated || busy;
    $('preflight').textContent = state.authorization === 'waiting' ? 'Waiting for your authorization' : checking === 'preflight' || state.phase === 'preflight' ? 'Checking setup…' : 'Run setup check';
    document.querySelectorAll('[data-next]').forEach((button) => { button.disabled = !authenticated || busy || (screen === 0 && supported() && !prerequisitesReady()); });
    document.querySelectorAll('[data-back]').forEach(button => { button.disabled = busy; });
  }
  async function api(path, body) {
    const response = await fetch(path, { method: body === undefined ? 'GET' : 'POST', credentials: 'same-origin', cache: 'no-store', headers: body === undefined ? {} : { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    const result = await response.json();
    if (!response.ok) throw new Error(result.error || 'The local installer could not complete this request.');
    return result;
  }
  function revealAuthorization() {
    $('authorization').focus({ preventScroll: true });
    $('authorization').scrollIntoView({ block: 'center' });
  }
  $('show-authorization').addEventListener('click', revealAuthorization);
  function receive(next) {
    document.documentElement.dataset.connection = 'connected';
    state = next; busy = Boolean(next.busy) || Boolean(checking); if (next.csrfToken) csrf = next.csrfToken;
    if (pendingPreflight && !next.busy && next.phase !== 'preflight') {
      if (next.phase === 'review' && next.checks?.length && next.checks.every(check => check.status === 'passed') && pendingPreflight === binding()) checkedDraft = pendingPreflight;
      pendingPreflight = '';
    }
    const selected = $('context').value;
    $('context').replaceChildren(new Option('Select a context', ''));
    for (const context of next.contexts || []) $('context').append(new Option(context.name + (context.current ? ' (current)' : ''), context.name));
    if (Array.from($('context').options).some((option) => option.value === selected)) $('context').value = selected;
    if (checking === 'prerequisites' || next.phase === 'prerequisites' || screen === 0) {
      const changed = next.prerequisiteSelection && !prerequisiteSelectionMatches();
      renderChecks('prerequisite-checks', changed ? [] : next.checks);
      $('prerequisite-message').textContent = changed ? 'Ports or components changed. Check prerequisites & ports again to continue.'
        : next.message || (busy ? 'Checking each prerequisite. No installation is running.' : 'Run the check to continue.');
    } else {
      renderChecks('preflight-checks', next.checks);
      if (screen === 3 && next.message) $('preflight-message').textContent = next.message;
    }
    renderChecks('install-checks', next.steps);
    $('live-resources').hidden = !next.observation;
    if (next.observation) {
      $('observation-message').textContent = next.observation.message;
      $('observation-time').textContent = next.observation.observedAt ? 'Last observed: ' + new Date(next.observation.observedAt).toLocaleTimeString() + (next.observation.status === 'live' ? ' · Refreshes every 3 seconds' : ' · Retained snapshot, not live') : 'No successful snapshot yet';
      renderChecks('resource-checks', next.observation.resources);
      $('resource-checks').classList.toggle('stale', next.observation.status !== 'live');
    }
    const waiting = next.authorization === 'waiting', terminalAuthorization = next.authorizationTarget === 'terminal';
    const newlyWaiting = waiting && ($('authorization').dataset.state !== 'waiting' || $('authorization').dataset.target !== (next.authorizationTarget || ''));
    const authorizationAnchor = screen === 3 ? $('preflight-checks') : $('run-status');
    if ($('authorization').parentElement !== authorizationAnchor.parentElement) authorizationAnchor.before($('authorization'));
    $('authorization').hidden = !next.authorization;
    $('authorization').dataset.state = next.authorization || '';
    $('authorization').dataset.target = next.authorizationTarget || '';
    document.documentElement.dataset.authorization = next.authorization || '';
    const authorizationHeading = terminalAuthorization ? 'Action required in Terminal' : 'Action required in the macOS dialog';
    const authorizationTitle = waiting ? authorizationHeading : next.authorization === 'approved' ? 'Authorization received' : 'Authorization did not complete';
    if ($('authorization-title').textContent !== authorizationTitle) $('authorization-title').textContent = authorizationTitle;
    const authorizationDetail = waiting
      ? terminalAuthorization
        ? 'Switch to the terminal window or IDE terminal panel where you ran kars up --setup-browser. At the Password: prompt, type your computer login password and press Enter. Nothing appears while you type; that is normal. This operation cannot continue until you approve. Keep this page open: it continues automatically, with no retry or new sign-in. Never enter your OS password in this browser. Press Ctrl+C in that terminal to cancel.'
        : 'Approve the certificate-trust request in the macOS system dialog. Check behind your browser if it is not visible. This operation cannot continue until you respond. Enter your OS password only in that system dialog. Keep this page open: it continues automatically after approval, with no retry or countdown. Deny there or press Ctrl+C in the launching terminal to stop.'
      : next.authorization === 'approved'
        ? screen === 3 ? 'Authorization succeeded. The setup check continues automatically; installation still requires your reviewed consent.' : 'Authorization succeeded. Continuing automatically; no further action is needed for this prompt.'
        : next.cleanup ? 'Cleanup authorization did not complete. Review the cleanup result and retained receipt; any partial cleanup remains recorded.' : 'Authorization was denied, cancelled, or failed. Setup stopped without retrying privileged operations or deleting resources; inspect the checks and any retained state below.';
    if ($('authorization-detail').textContent !== authorizationDetail) $('authorization-detail').textContent = authorizationDetail;
    $('attention').hidden = !waiting;
    const attentionMessage = waiting ? authorizationHeading + ' — return here after approval.' : '';
    if ($('attention-message').textContent !== attentionMessage) $('attention-message').textContent = attentionMessage;
    document.title = waiting ? authorizationHeading + ' · kars' : 'Set up your workspace · kars';
    if (screen === 4) {
      $('run-message').textContent = next.message || (busy ? 'The installer is working. Live checks will appear below.' : 'Review the recorded checks before continuing.');
      const ready = next.phase === 'ready';
      const previousStage = $('run-current').textContent, previousPhase = $('run-status').dataset.state;
      $('run-status').dataset.state = waiting ? 'waiting' : ready ? 'ready' : busy ? 'running' : 'failed';
      $('run-heading').textContent = waiting ? 'Waiting for your authorization.' : next.cleanup?.status === 'passed' ? 'Failed installation removed.' : next.cleanup?.status === 'running' ? 'Removing this attempt.' : ready ? 'Installed. Sign in next.' : busy ? 'Bringing it online.' : 'Setup needs attention.';
      $('return-review').hidden = busy || ready || Boolean(next.installation || next.recovery); $('shutdown').hidden = busy;
      const current = (next.steps || []).find(step => step.status === 'running' || step.status === 'failed');
      $('run-current').textContent = waiting ? authorizationHeading : next.cleanup ? 'Owned cleanup: ' + next.cleanup.status : current ? current.label : ready ? 'Local access is ready' : 'Installation stopped';
      const failure = next.observation?.resources.find(resource => resource.status === 'failed' || resource.status === 'blocked');
      $('run-detail').textContent = waiting ? 'Waiting for your response outside this browser. Approval resumes this operation automatically.' : next.cleanup ? next.cleanup.detail : current ? (current.status === 'failed' ? (failure ? failure.label + ': ' + failure.detail : current.detail + ' This stage did not complete; inspect the retained evidence.') : current.detail || '') : ready ? 'Sign in with the account you created. Customer workflow acceptance is still outstanding.' : failure ? failure.label + ': ' + failure.detail : 'No additional installation steps are running.';
      $('run-count').textContent = (next.steps || []).filter(step => step.status === 'passed').length + ' of ' + (next.steps || []).length + ' stages complete';
      $('run-progress').max = Math.max(1, (next.steps || []).length);
      $('run-progress').value = (next.steps || []).filter(step => step.status === 'passed').length;
      $('run-progress').hidden = !next.steps?.length;
      if (next.recovery && !next.installation) $('run-count').textContent = 'Cleanup-only session · installation cannot be resumed';
      if (previousStage !== $('run-current').textContent) animateChange($('run-current'), [{ opacity: .35 }, { opacity: 1 }], 240);
      if (ready && previousPhase !== 'ready') animateChange($('run-status'), [{ clipPath: 'inset(0 0 100% 0)' }, { clipPath: 'inset(0 0 0 0)' }], 480);
      $('recovery').hidden = !next.recovery;
      if (next.recovery) {
        const recovery = next.recovery, entries = [['Attempt directory', recovery.directory], ['Working directory', recovery.workingDirectory]];
        if (recovery.receipt === 'available') {
          entries.push(['Node ownership', next.cleanup?.status === 'passed' || (!recovery.nodeRecorded && recovery.cleanupAvailable) ? 'Owned node removed; failure receipt retained.' : recovery.nodeRecorded ? 'Recorded; resources were not automatically removed.' : 'No node identity recorded; automatic cleanup is not authorized.'],
            ['Host trust', recovery.hostTrustInstalled ? 'Recorded as installed or attempted; verify through owned teardown.' : 'No host-trust change recorded.']);
        } else entries.push(['Ownership receipt', recovery.receipt === 'absent' ? 'Not found. A cluster may not have been created; resource state is unverified.' : 'Could not be read. Resource state is unverified; do not delete by name.']);
        $('recovery-state').replaceChildren();
        for (const [label,value] of entries) { const dt = document.createElement('dt'), dd = document.createElement('dd'); dt.textContent = label; dd.textContent = value; $('recovery-state').append(dt,dd); }
        $('cleanup').hidden = !recovery.cleanupCommand;
        $('cleanup-command').textContent = recovery.cleanupCommand || '';
        $('owned-cleanup').hidden = !recovery.cleanupAvailable && !next.cleanup;
        $('cleanup-consent').textContent = 'Delete ' + (recovery.clusterName || next.installation?.clusterName || '') + ', all data inside its recorded node, and only its recorded hostname and certificate trust. Keep private receipts and leave other clusters and the Podman machine untouched.';
        $('confirm-cleanup').disabled = busy || !recovery.cleanupAvailable;
        $('run-cleanup').disabled = busy || !authenticated || !recovery.cleanupAvailable || !$('confirm-cleanup').checked;
        $('run-cleanup').textContent = waiting ? 'Waiting for your authorization' : next.cleanup?.status === 'running' ? 'Removing owned resources…' : next.cleanup?.status === 'failed' ? 'Retry owned cleanup' : next.cleanup?.status === 'passed' ? 'Cleanup complete' : 'Remove this failed installation';
        $('cleanup-status').textContent = waiting ? authorizationHeading + '. Cleanup continues automatically after approval.' : next.cleanup?.detail || 'Nothing is deleted until you confirm and click Remove.';
        $('recovery-guidance').textContent = recovery.cleanupCommand || next.cleanup?.status === 'passed' ? 'A fresh installation needs a new cluster name and a new review. Resolve the original failure first.' : 'Inspect the private attempt and container inventory with your operator before cleanup. Do not reuse this consumed attempt.';
      }
      updateElapsed();
      $('handoff').hidden = true;
      $('handoff').textContent = next.installation?.bridge ? 'Open Bridge →' : 'Open Headlamp →';
      if (ready && next.handoffUrl) {
        try { const url = new URL(next.handoffUrl); if ((url.protocol === 'https:' || (url.protocol === 'http:' && ['localhost','127.0.0.1','[::1]'].includes(url.hostname))) && !url.username && !url.password) { $('handoff').href = url.href; $('handoff').hidden = false; } } catch { /* Invalid handoff remains unavailable. */ }
      }
    }
    configureFields();
    if (newlyWaiting && $('authorization').getClientRects().length) revealAuthorization();
  }
  function schedulePoll() {
    clearTimeout(timer);
    if (!busy && !checking && !['starting','pending'].includes(state.copilot?.phase) && state.copilot?.modelsPhase !== 'loading') return;
    const generation = pollGeneration;
    timer = setTimeout(async () => { try {
      const next = await api('/api/state'); if (generation !== pollGeneration) return;
      receive(next); schedulePoll();
    } catch {
      if (generation !== pollGeneration) return;
      error('Connection to the local installer was lost. Keep the terminal open; do not restart installation blindly.');
      document.documentElement.dataset.connection = 'interrupted'; $('run-status').dataset.state = 'interrupted';
      $('run-heading').textContent = 'Connection interrupted.';
      $('attention').hidden = true;
      document.title = 'Connection interrupted · kars';
    } }, 750);
  }
  function stopPolling() { pollGeneration++; clearTimeout(timer); }
  function updateElapsed() {
    const installation = state.installation;
    if (!installation) { $('run-elapsed').textContent = ''; return; }
    const seconds = Math.max(0, Math.floor(((installation.finishedAt || Date.now()) - installation.startedAt) / 1000));
    $('run-elapsed').textContent = Math.floor(seconds / 60) + 'm ' + String(seconds % 60).padStart(2,'0') + 's elapsed';
  }
  setInterval(updateElapsed, 1000);
  $('confirm-cleanup').addEventListener('change', () => { $('run-cleanup').disabled = busy || !authenticated || !state.recovery?.cleanupAvailable || !$('confirm-cleanup').checked; });
  $('run-cleanup').addEventListener('click', async () => {
    const clusterName = state.recovery?.clusterName || state.installation?.clusterName;
    if ($('run-cleanup').disabled || !clusterName) return;
    const payload = { clusterName, confirm: true };
    $('confirm-cleanup').checked = false; $('run-cleanup').disabled = true;
    stopPolling(); busy = true; error(''); configureFields();
    try { receive(await api('/api/cleanup', payload)); schedulePoll(); }
    catch (err) {
      error(err.message);
      try { receive(await api('/api/state')); schedulePoll(); }
      catch { error('Cleanup status is unknown. Keep the terminal open and reconnect; do not delete by name or retry blindly.'); }
    }
  });
  $('generate-cluster').addEventListener('click', () => { if (!busy) { generateCluster(); invalidate(); configureFields(); } });
  $('copy-destination').addEventListener('click', () => {
    void copy(destination(), 'destination-feedback', 'Clipboard unavailable. Select the destination above and copy it manually.');
  });
  $('theme').addEventListener('click', () => { document.documentElement.dataset.theme = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'; });
  document.querySelectorAll('[data-next]').forEach((button) => button.addEventListener('click', () => { error(''); if (valid(screen)) show(screen === 0 && !supported() ? 3 : screen + 1); }));
  document.querySelectorAll('[data-back]').forEach((button) => button.addEventListener('click', () => { if (!busy) { error(''); show(screen === 3 && !supported() ? 0 : screen - 1); } }));
  function inputChanged(event) {
    if (event.target.name === 'inference') return;
    if (!['approve','trust','confirm-target','show-password'].includes(event.target.id)) invalidate();
    if (['https-port','bridge-port','headlamp-port','bridge'].includes(event.target.id)) {
      state.prerequisitesReady = false;
      renderChecks('prerequisite-checks', []);
      $('prerequisite-message').textContent = 'Ports or components changed. Check prerequisites & ports again to continue.';
    }
    if (['admin-password','admin-confirm'].includes(event.target.id)) passwordFeedback();
    if (['admin-email','admin-password','admin-confirm'].includes(event.target.id)) $('account-feedback').textContent = '';
    configureFields(); if (screen === 3) renderReview();
  }
  $('setup').addEventListener('input', inputChanged);
  $('setup').addEventListener('change', inputChanged);
  $('provider').addEventListener('change', async () => {
    invalidate(); $('api-key').value = ''; $('model').value = ''; $('copilot-model').value = '';
    $('endpoint').value = providerEndpoints[$('provider').value] || '';
    configureFields();
    if (state.copilot?.phase && state.copilot.phase !== 'idle') {
      try { receive(await api('/api/copilot/cancel', {})); } catch (err) { error(err.message); }
    }
  });
  document.querySelectorAll('input[name="inference"]').forEach(input => input.addEventListener('change', () => {
    if (!input.checked || input.disabled) return;
    $('provider').value = input.value;
    $('provider').dispatchEvent(new Event('change', { bubbles: true }));
  }));
  $('copilot-login').addEventListener('click', async () => {
    if (!authenticated || busy) return;
    invalidate(); error(''); $('code-feedback').textContent = ''; $('copilot-model').value = ''; $('copilot-login').disabled = true;
    try { receive(await api('/api/copilot/start', {})); schedulePoll(); } catch (err) { error(err.message); configureFields(); }
  });
  $('copilot-cancel').addEventListener('click', async () => {
    invalidate();
    try { receive(await api('/api/copilot/cancel', {})); schedulePoll(); } catch (err) { error(err.message); }
  });
  $('refresh-models').addEventListener('click', async () => {
    if (!authenticated || busy) return;
    invalidate(); $('copilot-model').value = ''; $('refresh-models').disabled = true;
    try { receive(await api('/api/copilot/models', {})); schedulePoll(); } catch (err) { error(err.message); configureFields(); }
  });
  $('copy-code').addEventListener('click', () => {
    const auth = state.copilot;
    if (auth?.phase === 'pending' && auth.userCode && auth.expiresAt > Date.now()) void copy(auth.userCode, 'code-feedback', 'Clipboard unavailable. Select the code above and copy it manually.');
  });
  $('generate-account').addEventListener('click', () => {
    if (busy) return;
    invalidate();
    const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!_';
    let password;
    do { password = Array.from(crypto.getRandomValues(new Uint8Array(24)), byte => alphabet[byte & 63]).join(''); } while (passwordIssue(password));
    const suffix = Array.from(crypto.getRandomValues(new Uint8Array(4)), byte => byte.toString(16).padStart(2,'0')).join('');
    $('admin-email').value = 'test-' + suffix + '@kars.local'; $('admin-password').value = password; $('admin-confirm').value = password;
    passwordFeedback(); configureFields();
    $('account-feedback').textContent = 'Unique test credentials generated. Copy them before installation; setup clears the password once the attempt is accepted.';
  });
  $('copy-account').addEventListener('click', () => {
    passwordFeedback();
    if (!$('admin-email').reportValidity() || !$('admin-password').reportValidity() || !$('admin-confirm').reportValidity()) return;
    void copy('Kars destination: ' + destination() + '\nEmail: ' + $('admin-email').value + '\nPassword: ' + $('admin-password').value, 'account-feedback', 'Clipboard unavailable. Use Show passwords and copy the account fields manually somewhere private.');
  });
  $('show-password').addEventListener('change', () => { for (const id of ['admin-password','admin-confirm']) $(id).type = $('show-password').checked ? 'text' : 'password'; });
  $('prerequisites').addEventListener('click', async () => {
    if (!authenticated || busy) return;
    configureFields();
    for (const input of $('local-ports').querySelectorAll('input')) {
      if (!input.disabled && !input.checkValidity()) { $('local-ports').open = true; input.reportValidity(); return; }
    }
    invalidate(); error(''); stopPolling(); checking = 'prerequisites'; busy = true; state.prerequisitesReady = false;
    renderChecks('prerequisite-checks', []); $('prerequisite-message').textContent = 'Checking selected ports, tools and your local Podman service…';
    configureFields(); schedulePoll();
    try {
      const next = await api('/api/prerequisites', { bridge: $('bridge').checked, ports: ports() }); stopPolling(); checking = ''; receive(next);
      if (next.checks.some(check => check.id.startsWith('port-') && check.status === 'blocked')) $('local-ports').open = true;
    }
    catch (err) { error(err.message); state.prerequisitesReady = false; $('prerequisite-message').textContent = 'Prerequisite checks did not complete. Retry after resolving the error.'; }
    finally { stopPolling(); checking = ''; busy = Boolean(state.busy); configureFields(); schedulePoll(); }
  });
  $('preflight').addEventListener('click', async () => {
    if (!authenticated || busy) return;
    for (let i = 0; i < 3; i++) { if (!valid(i)) { show(i); return; } }
    error(''); invalidate(); stopPolling(); checking = 'preflight'; busy = true; configureFields(); schedulePoll();
    $('preflight-message').textContent = 'Checking your setup. No installation changes are being made.';
    try {
      const approved = binding(); const next = await api('/api/preflight', draft()); stopPolling(); checking = ''; pendingPreflight = approved; receive(next);
      $('preflight-message').textContent = next.message || (next.busy ? 'Checking your setup. Authorization continues here automatically.' : checkedDraft ? 'Checks passed. Review and approve the changes below.' : 'Resolve the checks above before installing.');
    } catch (err) { error(err.message); $('preflight-message').textContent = 'Preflight did not pass. No installation was started.'; }
    finally { stopPolling(); checking = ''; busy = Boolean(state.busy); configureFields(); schedulePoll(); }
  });
  $('setup').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (screen !== 3 || $('install').disabled) return;
    const payload = draft(); stopPolling(); busy = true; updateInstall(); error(''); show(4);
    $('run-message').textContent = 'Request sent. Keep this tab and the launching terminal open.';
    $('run-current').textContent = 'Requesting installation…';
    $('run-detail').textContent = 'Checking the approved plan before making changes.';
    try {
      receive(await api('/api/install', payload));
      // Passwords are no longer needed in the DOM after the server accepts this attempt.
      ['api-key','admin-password','admin-confirm'].forEach((id) => { $(id).value = ''; });
      checkedDraft = ''; schedulePoll();
    } catch (err) {
      checkedDraft = '';
      try {
        receive(await api('/api/state'));
        if (['installing','ready','failed','closed'].includes(state.phase)) ['api-key','admin-password','admin-confirm'].forEach((id) => { $(id).value = ''; });
        error(err.message + ' Server state has been refreshed; no retry was submitted.'); schedulePoll();
      } catch {
        busy = true; updateInstall(); $('return-review').hidden = true;
        error('Installation status is unknown. Keep the terminal open and reload this page to reconnect. Do not start another attempt.');
      }
    }
  });
  $('return-review').addEventListener('click', () => { invalidate(); show(3); });
  $('shutdown').addEventListener('click', async () => {
    try { await api('/api/shutdown', {}); authenticated = false; document.documentElement.dataset.connection = 'closed'; $('connection').textContent = 'Installer closed'; $('shutdown').disabled = true; $('run-note').textContent = 'The local installer is closed. Installed components were not removed.'; updateInstall(); } catch (err) { error(err.message); }
  });
  async function connect() {
    const fragment = location.hash.slice(1), token = new URLSearchParams(fragment).get('token') || fragment;
    history.replaceState(null, '', location.pathname + location.search);
    try {
      if (token) { const session = await api('/api/session', { token }); csrf = session.csrfToken || ''; }
      const next = await api('/api/state'); csrf = next.csrfToken || csrf;
      if (!csrf) throw new Error('Use the private setup link from the launching terminal to authenticate.');
      authenticated = true; $('connection').textContent = 'Local session secured';
      if (next.phase === 'prerequisites') show(0);
      else if (next.phase === 'preflight') show(3);
      else if (next.busy || next.phase === 'ready' || next.phase === 'failed' || next.phase === 'closed') show(4);
      receive(next);
      schedulePoll();
    } catch { error('Open the private setup link printed by the Kars CLI. It authenticates this browser to the local installer. If the session expired, return to the terminal.'); $('connection').textContent = 'Authentication required'; }
    updateInstall();
  }
  $('endpoint').value = providerEndpoints[$('provider').value] || '';
  generateCluster(); configureFields(); connect();
})();
</script>
</body>
</html>`;
}
