// kars Bridge BFF — launch options projections.

use kube::core::DynamicObject;

use crate::providers::signing::sha256_hex;

use super::qualification::{
    mcp_resource_selection, memory_resource_selection, resource_qualification_routes,
    skill_resource_selection,
};
use super::{RefOption, bool_at, name_of, ns_of, readiness_summary, string_array_at, string_at};

pub(crate) fn mcp_server_option(resource: &DynamicObject) -> RefOption {
    let mode = string_at(&resource.data, &["/status/mode"]).or_else(|| {
        bool_at(&resource.data, &["/spec/managed"])
            .map(|managed| if managed { "Managed" } else { "External" }.to_string())
    });
    let discovered_tools = string_array_at(&resource.data, &["/status/discoveredTools"]);
    let tool_schema_digest =
        string_at(&resource.data, &["/status/toolSchemaDigest"]).or_else(|| {
            let signature = serde_json::json!({
                "mode": mode.clone(),
                "endpoint": string_at(
                    &resource.data,
                    &["/status/endpoint", "/spec/url", "/spec/endpoint"],
                ),
                "allowed_tools": string_array_at(&resource.data, &["/spec/allowedTools"]),
                "discovered_tools": discovered_tools.clone(),
            });
            serde_json::to_vec(&signature)
                .ok()
                .map(|bytes| format!("sha256:{}", sha256_hex(bytes)))
        });
    let mut option = RefOption {
        name: name_of(resource),
        namespace: ns_of(resource),
        summary: string_at(
            &resource.data,
            &["/status/endpoint", "/spec/url", "/spec/endpoint"],
        ),
        mode,
        discovered_tools,
        tool_schema_digest,
        compiled_digest: None,
        backend: None,
        readiness: readiness_summary(resource),
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: Vec::new(),
    };
    option.qualified_routes =
        resource_qualification_routes(&mcp_resource_selection(&option)).unwrap_or_default();
    option
}

pub(crate) fn memory_option(resource: &DynamicObject) -> RefOption {
    let mut option = RefOption {
        name: name_of(resource),
        namespace: ns_of(resource),
        summary: string_at(
            &resource.data,
            &["/spec/displayName", "/spec/storeName", "/status/storeName"],
        ),
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: None,
        compiled_digest: string_at(
            &resource.data,
            &[
                "/status/compiledDigest",
                "/status/compiled/digest",
                "/status/resolvedDigest",
                "/status/specDigest",
            ],
        ),
        backend: string_at(
            &resource.data,
            &[
                "/status/backend",
                "/spec/backend",
                "/status/binding/backend",
                "/spec/binding/backend",
                "/status/provider",
                "/spec/provider",
            ],
        )
        .or_else(|| Some("foundry".into())),
        readiness: readiness_summary(resource),
        version: None,
        recipe: None,
        version_digest: None,
        qualified_routes: Vec::new(),
    };
    option.qualified_routes =
        resource_qualification_routes(&memory_resource_selection(&option)).unwrap_or_default();
    option
}

pub(crate) fn skill_option(resource: &DynamicObject) -> RefOption {
    let mut option = RefOption {
        name: name_of(resource),
        namespace: ns_of(resource),
        summary: string_at(&resource.data, &["/spec/summary"]),
        mode: None,
        discovered_tools: Vec::new(),
        tool_schema_digest: None,
        compiled_digest: None,
        backend: None,
        readiness: string_at(&resource.data, &["/status/phase"]),
        version: string_at(&resource.data, &["/spec/version"]),
        recipe: string_at(&resource.data, &["/spec/recipe"]),
        version_digest: string_at(&resource.data, &["/status/versionDigest"]),
        qualified_routes: Vec::new(),
    };
    option.qualified_routes =
        resource_qualification_routes(&skill_resource_selection(&option)).unwrap_or_default();
    option
}
