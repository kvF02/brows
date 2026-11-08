"""CI-only metadata: never read or print a cookie value or encryption key."""
import json
import os
import pathlib
import sqlite3
import sys

assert os.environ.get("GITHUB_ACTIONS") == "true"
assert os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
root = pathlib.Path(sys.argv[1])
if str(root).startswith(chr(92) * 2 + "?" + chr(92)):
    root = pathlib.Path(str(root)[4:])
assert root.name == "userdata" and any(p.name.startswith("brows-login-") for p in root.parents)
result = {}
for filename, label in [("Default/Preferences", "preferences"), ("Local State", "localState")]:
    try:
        value = json.loads((root / filename).read_text(encoding="utf-8"))
        result[label] = ({"exitType": value.get("profile", {}).get("exit_type")}
                         if label == "preferences" else
                         {"hasEncryptedKey": bool(value.get("os_crypt", {}).get("encrypted_key")),
                          "hasAppBoundEncryptedKey": bool(value.get("os_crypt", {}).get("app_bound_encrypted_key"))})
    except Exception as error:
        result[label] = {"error": type(error).__name__}
result["cookiesDatabaseExists"] = False
result["stores"] = {}
for relative in ["Default/Network/Cookies", "Default/Cookies", "Default/History", "Default/Login Data"]:
    database = root / relative
    result["stores"][relative] = database.is_file()
    if not database.is_file() or not relative.endswith("Cookies"):
        continue
    result["cookiesDatabaseExists"] = True
    try:
        with sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True) as connection:
            result["cookieMetadata"] = connection.execute(
                "SELECT is_persistent, is_secure, is_httponly, has_expires, length(encrypted_value), length(value) "
                "FROM cookies WHERE name = ?", ("__Host-brows-fixture",)).fetchall()
    except Exception as error:
        result["databaseError"] = type(error).__name__
print(json.dumps(result))
