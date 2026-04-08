import { describe, it, expect } from "vitest";
import path from "path";

/**
 * Tests for the `push` command's data-transformation logic.
 *
 * The push command builds docker images and pushes to ACR. We test:
 * - Image tag generation
 * - --only flag filtering
 * - Docker build argument construction
 * - ACR login server resolution
 */

// --- Helpers that mirror the logic in push.ts ---

interface ImageDef {
  name: string;
  tag: string;
  dockerfile: string;
  context?: string;
  buildArgs?: string[];
}

function buildImageList(acrLoginServer: string): ImageDef[] {
  const now = 1700000000000; // fixed timestamp for tests
  return [
    { name: "controller", tag: "azureclaw-controller:latest", dockerfile: "controller/Dockerfile" },
    {
      name: "router",
      tag: "azureclaw-inference-router:latest",
      dockerfile: "inference-router/Dockerfile",
      buildArgs: ["--build-arg", `ROUTER_CACHE_BUST=${now}`],
    },
    {
      name: "sandbox",
      tag: "openclaw-sandbox:latest",
      dockerfile: "sandbox-images/openclaw/Dockerfile",
      buildArgs: [
        "--build-arg",
        `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/azureclaw-inference-router:latest`,
        "--build-arg",
        `OPENCLAW_CACHE_BUST=${now}`,
      ],
    },
    {
      name: "relay",
      tag: "agentmesh-relay:latest",
      dockerfile: "vendor/agentmesh-relay/Dockerfile",
      context: "vendor/agentmesh-relay",
      buildArgs: ["--build-arg", `CACHE_BUST=${now}`],
    },
    {
      name: "registry",
      tag: "agentmesh-registry:latest",
      dockerfile: "vendor/agentmesh-registry/Dockerfile",
      context: "vendor/agentmesh-registry",
      buildArgs: ["--build-arg", `CACHE_BUST=${now}`],
    },
  ];
}

function filterImages(images: ImageDef[], only?: string): ImageDef[] {
  return only ? images.filter((i) => i.name === only) : images;
}

function resolveAcr(acrName?: string, ctxAcrName?: string) {
  const name = acrName || ctxAcrName;
  return name ? { acrName: name, acrLoginServer: `${name}.azurecr.io` } : null;
}

function buildDockerArgs(img: ImageDef, acrLoginServer: string, repoRoot: string): string[] {
  return [
    "build",
    "--platform",
    "linux/amd64",
    "--provenance=false",
    "--sbom=false",
    "-f",
    path.join(repoRoot, img.dockerfile),
    "-t",
    `${acrLoginServer}/${img.tag}`,
    ...(img.buildArgs || []),
    img.context ? path.join(repoRoot, img.context) : repoRoot,
  ];
}

/** Deployment map for --apply restart logic */
const deploymentMap: Record<string, string> = {
  controller: "azureclaw-controller",
  router: "azureclaw-controller",
  sandbox: "azureclaw-controller",
};

// --- Tests ---

describe("ACR login server resolution", () => {
  it("resolves from explicit --acr flag", () => {
    const result = resolveAcr("myacr", undefined);
    expect(result).toEqual({ acrName: "myacr", acrLoginServer: "myacr.azurecr.io" });
  });

  it("falls back to context acrName", () => {
    const result = resolveAcr(undefined, "ctxacr");
    expect(result).toEqual({ acrName: "ctxacr", acrLoginServer: "ctxacr.azurecr.io" });
  });

  it("prefers explicit flag over context", () => {
    const result = resolveAcr("flagacr", "ctxacr");
    expect(result).toEqual({ acrName: "flagacr", acrLoginServer: "flagacr.azurecr.io" });
  });

  it("returns null when no ACR configured", () => {
    expect(resolveAcr(undefined, undefined)).toBeNull();
  });
});

describe("image list", () => {
  it("defines 5 images", () => {
    const images = buildImageList("test.azurecr.io");
    expect(images).toHaveLength(5);
  });

  it("includes all expected image names", () => {
    const images = buildImageList("test.azurecr.io");
    const names = images.map((i) => i.name);
    expect(names).toEqual(["controller", "router", "sandbox", "relay", "registry"]);
  });

  it("sandbox image references router image from ACR", () => {
    const images = buildImageList("myacr.azurecr.io");
    const sandbox = images.find((i) => i.name === "sandbox")!;
    expect(sandbox.buildArgs).toContain(
      "--build-arg",
    );
    const routerArg = sandbox.buildArgs!.find((a) =>
      a.includes("INFERENCE_ROUTER_IMAGE="),
    );
    expect(routerArg).toBe(
      "INFERENCE_ROUTER_IMAGE=myacr.azurecr.io/azureclaw-inference-router:latest",
    );
  });

  it("relay and registry have custom context paths", () => {
    const images = buildImageList("test.azurecr.io");
    expect(images.find((i) => i.name === "relay")!.context).toBe("vendor/agentmesh-relay");
    expect(images.find((i) => i.name === "registry")!.context).toBe("vendor/agentmesh-registry");
  });

  it("controller has no buildArgs", () => {
    const images = buildImageList("test.azurecr.io");
    const ctrl = images.find((i) => i.name === "controller")!;
    expect(ctrl.buildArgs).toBeUndefined();
  });
});

describe("--only flag filtering", () => {
  const images = buildImageList("test.azurecr.io");

  it("returns all images when --only is not set", () => {
    expect(filterImages(images)).toHaveLength(5);
  });

  it("filters to single image when --only is set", () => {
    const result = filterImages(images, "controller");
    expect(result).toHaveLength(1);
    expect(result[0].name).toBe("controller");
  });

  it("returns empty for unknown image name", () => {
    expect(filterImages(images, "nonexistent")).toHaveLength(0);
  });

  it("filters router correctly", () => {
    const result = filterImages(images, "router");
    expect(result).toHaveLength(1);
    expect(result[0].tag).toBe("azureclaw-inference-router:latest");
  });

});

describe("docker build command construction", () => {
  const acrLoginServer = "myacr.azurecr.io";
  const repoRoot = "/home/user/azureclaw";

  it("builds correct args for controller (no buildArgs, no context)", () => {
    const img: ImageDef = {
      name: "controller",
      tag: "azureclaw-controller:latest",
      dockerfile: "controller/Dockerfile",
    };
    const args = buildDockerArgs(img, acrLoginServer, repoRoot);
    expect(args[0]).toBe("build");
    expect(args).toContain("--platform");
    expect(args).toContain("linux/amd64");
    expect(args).toContain("-f");
    expect(args).toContain(path.join(repoRoot, "controller/Dockerfile"));
    expect(args).toContain("-t");
    expect(args).toContain("myacr.azurecr.io/azureclaw-controller:latest");
    // Last arg is the build context (repoRoot since no custom context)
    expect(args[args.length - 1]).toBe(repoRoot);
  });

  it("includes buildArgs for router", () => {
    const img: ImageDef = {
      name: "router",
      tag: "azureclaw-inference-router:latest",
      dockerfile: "inference-router/Dockerfile",
      buildArgs: ["--build-arg", "ROUTER_CACHE_BUST=123"],
    };
    const args = buildDockerArgs(img, acrLoginServer, repoRoot);
    expect(args).toContain("--build-arg");
    expect(args).toContain("ROUTER_CACHE_BUST=123");
  });

  it("uses custom context path for relay", () => {
    const img: ImageDef = {
      name: "relay",
      tag: "agentmesh-relay:latest",
      dockerfile: "vendor/agentmesh-relay/Dockerfile",
      context: "vendor/agentmesh-relay",
      buildArgs: ["--build-arg", "CACHE_BUST=123"],
    };
    const args = buildDockerArgs(img, acrLoginServer, repoRoot);
    expect(args[args.length - 1]).toBe(path.join(repoRoot, "vendor/agentmesh-relay"));
  });

  it("always includes --provenance=false and --sbom=false", () => {
    const img: ImageDef = {
      name: "controller",
      tag: "azureclaw-controller:latest",
      dockerfile: "controller/Dockerfile",
    };
    const args = buildDockerArgs(img, acrLoginServer, repoRoot);
    expect(args).toContain("--provenance=false");
    expect(args).toContain("--sbom=false");
  });

  it("generates full ACR tag with login server prefix", () => {
    const img: ImageDef = {
      name: "relay",
      tag: "agentmesh-relay:latest",
      dockerfile: "vendor/agentmesh-relay/Dockerfile",
      context: "vendor/agentmesh-relay",
    };
    const args = buildDockerArgs(img, acrLoginServer, repoRoot);
    expect(args).toContain("myacr.azurecr.io/agentmesh-relay:latest");
  });
});

describe("--apply restart logic", () => {
  it("maps controller to azureclaw-controller deployment", () => {
    expect(deploymentMap.controller).toBe("azureclaw-controller");
  });

  it("maps router to azureclaw-controller (router in sandbox pods)", () => {
    expect(deploymentMap.router).toBe("azureclaw-controller");
  });

  it("maps sandbox to azureclaw-controller (controller manages sandbox pods)", () => {
    expect(deploymentMap.sandbox).toBe("azureclaw-controller");
  });
});
