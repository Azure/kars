# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Patch Hermes 0.16.0 Feishu admission to honor the Kars channel contract."""

from __future__ import annotations

import importlib.util
from pathlib import Path


def replace_once(source: str, old: str, new: str) -> str:
    if source.count(old) != 1:
        raise RuntimeError(f"expected one Hermes Feishu source anchor, found {source.count(old)}")
    return source.replace(old, new)


def main() -> None:
    spec = importlib.util.find_spec("gateway.platforms.feishu")
    if spec is None or spec.origin is None:
        raise RuntimeError("Hermes Feishu adapter is not installed")

    gateway_spec = importlib.util.find_spec("gateway.run")
    if gateway_spec is None or gateway_spec.origin is None:
        raise RuntimeError("Hermes gateway runner is not installed")
    gateway_source = Path(gateway_spec.origin).read_text()
    pairing_contract = (
        "def _adapter_dm_policy",
        "pairing_store.is_approved",
        'self._adapter_dm_policy(source.platform) == "pairing"',
        "self.pairing_store.generate_code",
    )
    if any(anchor not in gateway_source for anchor in pairing_contract):
        raise RuntimeError("Hermes gateway pairing contract is incompatible")

    path = Path(spec.origin)
    source = path.read_text()
    source = replace_once(
        source,
        'class FeishuAdapter(BasePlatformAdapter):\n    """Feishu/Lark bot adapter."""\n',
        'class FeishuAdapter(BasePlatformAdapter):\n'
        '    """Feishu/Lark bot adapter."""\n\n'
        '    enforces_own_access_policy = True\n'
        '    _kars_ready_path = Path("/tmp/kars-channel-feishu-ready")\n',
    )
    source = replace_once(
        source,
        "    group_policy: str\n    allowed_group_users: frozenset[str]\n",
        "    dm_policy: str\n    dm_allow_from: frozenset[str]\n"
        "    group_policy: str\n    allowed_group_users: frozenset[str]\n",
    )
    source = replace_once(
        source,
        '            group_policy=os.getenv("FEISHU_GROUP_POLICY", "allowlist").strip().lower(),\n',
        '            dm_policy=str(extra.get("dm_policy") or os.getenv("FEISHU_DM_POLICY", "pairing")).strip().lower(),\n'
        '            dm_allow_from=frozenset(str(item).strip() for item in extra.get("dm_allow_from", []) if str(item).strip()),\n'
        '            group_policy=os.getenv("FEISHU_GROUP_POLICY", "allowlist").strip().lower(),\n',
    )
    source = replace_once(
        source,
        "        self._group_policy = settings.group_policy\n",
        "        self._dm_policy = settings.dm_policy\n"
        "        self._dm_allow_from = set(settings.dm_allow_from)\n"
        "        self._group_policy = settings.group_policy\n",
    )
    source = replace_once(
        source,
        "        if not is_group:\n            return None\n",
        "        if not is_group:\n"
        "            if self._dm_policy == \"disabled\":\n"
        "                return \"dm_policy_rejected\"\n"
        "            if self._dm_policy == \"allowlist\":\n"
        "                if not sender_ids or not (sender_ids & self._dm_allow_from):\n"
        "                    return \"dm_policy_rejected\"\n"
        "            if self._dm_policy not in {\"pairing\", \"allowlist\", \"disabled\"}:\n"
        "                return \"dm_policy_rejected\"\n"
        "            return None\n",
    )
    source = replace_once(
        source,
        "    async def disconnect(self) -> None:\n"
        "        \"\"\"Disconnect from Feishu/Lark.\"\"\"\n"
        "        self._running = False\n",
        "    async def disconnect(self) -> None:\n"
        "        \"\"\"Disconnect from Feishu/Lark.\"\"\"\n"
        "        self._running = False\n"
        "        self._kars_ready_path.unlink(missing_ok=True)\n",
    )
    source = replace_once(
        source,
        "        except Exception as exc:\n            await self._release_app_lock()\n",
        "        except Exception as exc:\n"
        "            self._kars_ready_path.unlink(missing_ok=True)\n"
        "            await self._release_app_lock()\n",
    )
    source = replace_once(
        source,
        "            except Exception as exc:\n"
        "                self._running = False\n"
        "                self._disable_websocket_auto_reconnect()\n",
        "            except Exception as exc:\n"
        "                self._running = False\n"
        "                self._kars_ready_path.unlink(missing_ok=True)\n"
        "                self._disable_websocket_auto_reconnect()\n",
    )
    path.write_text(source)

    sdk_spec = importlib.util.find_spec("lark_oapi")
    if sdk_spec is None or sdk_spec.submodule_search_locations is None:
        raise RuntimeError("lark-oapi WebSocket client is not installed")

    sdk_root = next(iter(sdk_spec.submodule_search_locations), None)
    if sdk_root is None:
        raise RuntimeError("lark-oapi package directory is unavailable")
    sdk_path = Path(sdk_root) / "ws" / "client.py"
    sdk_source = sdk_path.read_text()
    sdk_source = replace_once(
        sdk_source,
        "import time\nfrom urllib.parse import urlparse, parse_qs\n",
        "import time\nfrom pathlib import Path\nfrom urllib.parse import urlparse, parse_qs\n\n"
        '_KARS_READY_PATH = Path("/tmp/kars-channel-feishu-ready")\n',
    )
    sdk_source = replace_once(
        sdk_source,
        "            self._conn = conn\n            self._conn_url = conn_url\n",
        "            self._conn = conn\n"
        "            _KARS_READY_PATH.touch(mode=0o600, exist_ok=True)\n"
        "            self._conn_url = conn_url\n",
    )
    sdk_source = replace_once(
        sdk_source,
        "        finally:\n            self._conn = None\n            self._conn_url = \"\"\n",
        "        finally:\n"
        "            _KARS_READY_PATH.unlink(missing_ok=True)\n"
        "            self._conn = None\n"
        "            self._conn_url = \"\"\n",
    )
    sdk_path.write_text(sdk_source)


if __name__ == "__main__":
    main()
