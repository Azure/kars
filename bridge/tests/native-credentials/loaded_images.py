"""Pin Kind-imported Docker images using containerd's actual manifest identity."""

import json

from native_api import command, require


def manifest_digest(table, reference):
    rows = [line.split() for line in table.splitlines()]
    matches = [row for row in rows if row and row[0] == reference]
    require(len(matches) == 1 and len(matches[0]) >= 3,
            "Loaded containerd image identity missing or ambiguous")
    digest = matches[0][2]
    require(digest.startswith("sha256:") and len(digest) == 71
            and all(character in "0123456789abcdef" for character in digest[7:]),
            "Loaded containerd manifest digest is malformed")
    return digest


def loaded_image(name):
    repository = f"docker.io/library/{name}"
    tagged = f"{repository}:latest"
    expected = None
    for node in ["bridge-native-control-plane", "bridge-native-worker"]:
        table = command("docker", "exec", node, "ctr", "--namespace", "k8s.io",
                        "images", "list", f"name=={json.dumps(tagged)}")
        digest = manifest_digest(table, tagged)
        require(expected is None or expected == digest, "Kind nodes loaded different image content")
        expected = digest
        reference = f"{repository}@{digest}"
        before = json.loads(command("docker", "exec", node, "crictl", "inspecti", tagged))
        # Docker archives imported by Kind can have empty CRI repoDigests.
        # Add the exact digest alias to local containerd metadata, never pull,
        # republish, or substitute the image's config hash for its manifest.
        aliases = command("docker", "exec", node, "ctr", "--namespace", "k8s.io",
                          "images", "list", f"name=={json.dumps(reference)}")
        if any(line.split() and line.split()[0] == reference for line in aliases.splitlines()):
            require(manifest_digest(aliases, reference) == digest, "Existing image alias differs")
        else:
            command("docker", "exec", node, "ctr", "--namespace", "k8s.io",
                    "images", "tag", tagged, reference)
        after = json.loads(command("docker", "exec", node, "crictl", "inspecti", reference))
        require(after["status"]["id"] == before["status"]["id"],
                "Digest alias did not resolve to the original loaded image")
    algorithm, digest = expected.split(":", 1)
    # The chart concatenates repository:tag. This renders a digest-only image;
    # it avoids :latest => Always defaulting on the router's native Pod schema.
    return {"repository": f"{repository}@{algorithm}", "tag": digest, "pullPolicy": "IfNotPresent"}
