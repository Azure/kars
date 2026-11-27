// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { copilotDeviceLogin } from "../../github-copilot.js";
import { SetupError, type SetupCopilotState } from "./types.js";
import { discoverSetupCopilotModels } from "./providers.js";

/** Keeps OAuth credentials in the installer process, never in browser state. */
export class SetupCopilotLogin {
  readonly state: SetupCopilotState = { phase: "idle" };
  private controller?: AbortController;
  private modelsController?: AbortController;
  private token?: string;
  constructor(private readonly login: typeof copilotDeviceLogin = copilotDeviceLogin,
    private readonly discover: typeof discoverSetupCopilotModels = discoverSetupCopilotModels) {}

  start(): void {
    this.cancel();
    const controller = new AbortController();
    this.controller = controller;
    this.state.phase = "starting";
    void this.login(info => {
      if (controller.signal.aborted) return;
      Object.assign(this.state, { phase: "pending", userCode: info.userCode, verificationUri: info.verificationUri, expiresAt: Date.now() + info.expiresInSec * 1000 });
    }, { signal: controller.signal }).then(token => {
      if (controller.signal.aborted) return;
      this.clearPublic();
      this.token = token;
      this.state.phase = "connected";
      this.refreshModels();
    }).catch(() => {
      if (controller.signal.aborted) return;
      this.clearPublic();
      this.state.phase = "failed";
      this.state.message = "GitHub sign-in failed, expired or was declined. Retry sign-in; no credentials or provider errors are displayed.";
    });
  }

  credential(model?: string): string {
    if (!this.token || this.state.phase !== "connected") throw new SetupError("Sign in with GitHub Copilot before running preflight");
    if (model !== undefined && (this.state.modelsPhase !== "ready" || !this.state.models?.some(item => item.id === model))) throw new SetupError("Choose a model from the current Copilot account's model list, then run preflight");
    return this.token;
  }

  refreshModels(): void {
    const token = this.credential();
    this.modelsController?.abort();
    const controller = new AbortController(); this.modelsController = controller;
    delete this.state.models; delete this.state.modelsMessage;
    this.state.modelsPhase = "loading";
    void this.discover(token, controller.signal).then(models => {
      if (controller.signal.aborted) return;
      this.state.models = models; this.state.modelsPhase = "ready";
    }).catch(() => {
      if (controller.signal.aborted) return;
      this.state.modelsPhase = "failed";
      this.state.modelsMessage = "Could not load chat models. Check your Copilot subscription and organization model policy, then retry loading models.";
    });
  }

  cancel(): void {
    this.controller?.abort(); this.controller = undefined; this.token = undefined;
    this.modelsController?.abort(); this.modelsController = undefined;
    delete this.state.models; delete this.state.modelsPhase; delete this.state.modelsMessage;
    this.clearPublic(); this.state.phase = "idle";
  }
  private clearPublic(): void {
    delete this.state.userCode; delete this.state.verificationUri;
    delete this.state.expiresAt; delete this.state.message;
  }
}
