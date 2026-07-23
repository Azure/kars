from pathlib import Path

import agent.model_metadata as model_metadata


path = Path(model_metadata.__file__)
source = path.read_text()
needle = '''def fetch_model_metadata(force_refresh: bool = False) -> Dict[str, Dict[str, Any]]:
    """Fetch model metadata from OpenRouter (cached for 1 hour)."""
'''
replacement = needle + '''    if os.getenv("HERMES_DISABLE_OPENROUTER_METADATA", "").lower() in {"1", "true", "yes"}:
        return {}
'''
if source.count(needle) != 1:
    raise SystemExit(f"unexpected Hermes model_metadata.py shape at {path}")
path.write_text(source.replace(needle, replacement))
