#!/usr/bin/env python
import json
import os
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any


PLUGIN_NAME = "google-workspace-manager"
DEFAULT_SCOPES = [
    "https://www.googleapis.com/auth/script.projects",
    "https://www.googleapis.com/auth/script.deployments",
    "https://www.googleapis.com/auth/script.scriptapp",
    "https://www.googleapis.com/auth/drive.metadata.readonly",
    "https://www.googleapis.com/auth/admin.directory.user.readonly",
]


def plugin_data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    if appdata:
        return Path(appdata) / "claw" / "plugins" / PLUGIN_NAME
    return Path.home() / ".claw" / "plugins" / PLUGIN_NAME


def state_file() -> Path:
    return plugin_data_dir() / "accounts.json"


def token_dir() -> Path:
    return plugin_data_dir() / "tokens"


def ensure_dirs() -> None:
    plugin_data_dir().mkdir(parents=True, exist_ok=True)
    token_dir().mkdir(parents=True, exist_ok=True)


def read_input() -> dict[str, Any]:
    if not sys.stdin.isatty():
        raw = sys.stdin.read().strip()
        if raw:
            return json.loads(raw)
    env_payload = os.environ.get("CLAWD_TOOL_INPUT", "").strip()
    return json.loads(env_payload) if env_payload else {}


@dataclass
class AccountProfile:
    account_name: str
    client_secret_path: str
    scopes: list[str] = field(default_factory=lambda: list(DEFAULT_SCOPES))
    admin_email: str | None = None
    is_default: bool = False


@dataclass
class StoredState:
    accounts: list[AccountProfile] = field(default_factory=list)


def load_state() -> StoredState:
    ensure_dirs()
    if not state_file().exists():
        return StoredState()
    payload = json.loads(state_file().read_text(encoding="utf-8"))
    return StoredState(
        accounts=[AccountProfile(**account) for account in payload.get("accounts", [])]
    )


def save_state(state: StoredState) -> None:
    ensure_dirs()
    state_file().write_text(
        json.dumps({"accounts": [asdict(account) for account in state.accounts]}, indent=2),
        encoding="utf-8",
    )


def ok(**payload: Any) -> None:
    print(json.dumps({"ok": True, **payload}, indent=2))


def fail(message: str, **payload: Any) -> None:
    print(json.dumps({"ok": False, "error": message, **payload}, indent=2))
    sys.exit(1)


def require_google_deps() -> tuple[Any, Any, Any, Any]:
    try:
        from google.oauth2.credentials import Credentials
        from google_auth_oauthlib.flow import InstalledAppFlow
        from google.auth.transport.requests import Request
        from googleapiclient.discovery import build
    except ImportError as error:
        fail(
            "Missing Google Python dependencies.",
            detail=str(error),
            install="python -m pip install -r tools/google_workspace_requirements.txt",
        )
    return Credentials, InstalledAppFlow, Request, build


def find_account(state: StoredState, account_name: str | None) -> AccountProfile:
    if account_name:
        for account in state.accounts:
            if account.account_name == account_name:
                return account
        fail(f"Unknown account '{account_name}'.")
    defaults = [account for account in state.accounts if account.is_default]
    if defaults:
        return defaults[0]
    if len(state.accounts) == 1:
        return state.accounts[0]
    fail("No default account is configured. Specify account_name explicitly.")


def token_path_for(account: AccountProfile) -> Path:
    safe_name = "".join(ch if ch.isalnum() or ch in ("-", "_") else "_" for ch in account.account_name)
    return token_dir() / f"{safe_name}.json"


def load_credentials(account: AccountProfile):
    Credentials, InstalledAppFlow, Request, _build = require_google_deps()
    path = token_path_for(account)
    creds = None
    if path.exists():
        creds = Credentials.from_authorized_user_file(str(path), account.scopes)
    if creds and creds.expired and creds.refresh_token:
        creds.refresh(Request())
        path.write_text(creds.to_json(), encoding="utf-8")
    if not creds or not creds.valid:
        fail(
            f"Account '{account.account_name}' is not logged in.",
            next_step="Run google_workspace_login for this account first.",
        )
    return creds


def configure_account(input_data: dict[str, Any]) -> None:
    account_name = str(input_data.get("account_name", "")).strip()
    client_secret_path = str(input_data.get("client_secret_path", "")).strip()
    if not account_name or not client_secret_path:
        fail("account_name and client_secret_path are required.")
    client_secret = Path(client_secret_path)
    if not client_secret.exists():
        fail(f"Client secret file does not exist: {client_secret}")

    state = load_state()
    scopes = input_data.get("scopes") or list(DEFAULT_SCOPES)
    set_default = bool(input_data.get("set_default", False))
    admin_email = input_data.get("admin_email")

    updated = False
    for account in state.accounts:
        if account.account_name == account_name:
            account.client_secret_path = str(client_secret.resolve())
            account.scopes = list(scopes)
            account.admin_email = admin_email
            account.is_default = set_default or account.is_default
            updated = True

    if not updated:
        state.accounts.append(
            AccountProfile(
                account_name=account_name,
                client_secret_path=str(client_secret.resolve()),
                scopes=list(scopes),
                admin_email=admin_email,
                is_default=set_default or not state.accounts,
            )
        )

    if set_default:
        for account in state.accounts:
            account.is_default = account.account_name == account_name

    save_state(state)
    ok(
        message=f"Saved account '{account_name}'.",
        account_name=account_name,
        default_account=next(
            (account.account_name for account in state.accounts if account.is_default), None
        ),
    )


def login(input_data: dict[str, Any]) -> None:
    account_name = str(input_data.get("account_name", "")).strip()
    state = load_state()
    account = find_account(state, account_name)
    _Credentials, InstalledAppFlow, _Request, _build = require_google_deps()
    flow = InstalledAppFlow.from_client_secrets_file(account.client_secret_path, account.scopes)
    creds = flow.run_local_server(port=0)
    token_path_for(account).write_text(creds.to_json(), encoding="utf-8")
    ok(
        message=f"Authenticated '{account.account_name}'.",
        account_name=account.account_name,
        token_file=str(token_path_for(account)),
    )


def list_accounts(_input_data: dict[str, Any]) -> None:
    state = load_state()
    ok(
        accounts=[
            {
                "account_name": account.account_name,
                "client_secret_path": account.client_secret_path,
                "admin_email": account.admin_email,
                "is_default": account.is_default,
                "logged_in": token_path_for(account).exists(),
            }
            for account in state.accounts
        ]
    )


def delete_account(input_data: dict[str, Any]) -> None:
    account_name = str(input_data.get("account_name", "")).strip()
    if not account_name:
        fail("account_name is required.")
    state = load_state()
    before = len(state.accounts)
    removed = [account for account in state.accounts if account.account_name == account_name]
    state.accounts = [account for account in state.accounts if account.account_name != account_name]
    if len(state.accounts) == before:
        fail(f"Unknown account '{account_name}'.")
    if state.accounts and not any(account.is_default for account in state.accounts):
        state.accounts[0].is_default = True
    save_state(state)
    for account in removed:
        token_path_for(account).unlink(missing_ok=True)
    ok(message=f"Deleted account '{account_name}'.")


def build_service(api_name: str, version: str, account_name: str | None):
    _Credentials, _InstalledAppFlow, _Request, build = require_google_deps()
    state = load_state()
    account = find_account(state, account_name)
    creds = load_credentials(account)
    return build(api_name, version, credentials=creds), account


def list_scripts(input_data: dict[str, Any]) -> None:
    drive, account = build_service("drive", "v3", input_data.get("account_name"))
    query = "mimeType='application/vnd.google-apps.script' and trashed=false"
    extra = str(input_data.get("query", "")).strip()
    if extra:
        escaped = extra.replace("'", "\\'")
        query = f"{query} and name contains '{escaped}'"
    response = (
        drive.files()
        .list(
            q=query,
            pageSize=int(input_data.get("page_size", 25)),
            fields="files(id,name,modifiedTime,owners(emailAddress))",
        )
        .execute()
    )
    ok(account_name=account.account_name, scripts=response.get("files", []))


def create_script_project(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    title = str(input_data.get("title", "")).strip()
    if not title:
        fail("title is required.")
    body = {"title": title}
    parent_id = input_data.get("parent_id")
    if parent_id:
        body["parentId"] = parent_id
    project = script.projects().create(body=body).execute()
    script_id = project["scriptId"]
    if input_data.get("files"):
        script.projects().updateContent(
            scriptId=script_id,
            body={"files": input_data["files"]},
        ).execute()
    ok(account_name=account.account_name, script_id=script_id, project=project)


def get_script_content(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    script_id = str(input_data.get("script_id", "")).strip()
    if not script_id:
        fail("script_id is required.")
    content = script.projects().getContent(scriptId=script_id).execute()
    ok(account_name=account.account_name, script_id=script_id, content=content)


def update_script_content(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    script_id = str(input_data.get("script_id", "")).strip()
    files = input_data.get("files")
    if not script_id or not files:
        fail("script_id and files are required.")
    result = script.projects().updateContent(
        scriptId=script_id,
        body={"files": files},
    ).execute()
    ok(account_name=account.account_name, script_id=script_id, updated=result)


def export_script_to_dir(input_data: dict[str, Any]) -> None:
    script_id = str(input_data.get("script_id", "")).strip()
    output_dir = str(input_data.get("output_dir", "")).strip()
    if not script_id or not output_dir:
        fail("script_id and output_dir are required.")
    script, account = build_service("script", "v1", input_data.get("account_name"))
    content = script.projects().getContent(scriptId=script_id).execute()
    target = Path(output_dir)
    target.mkdir(parents=True, exist_ok=True)
    written_files = []
    for entry in content.get("files", []):
        name = entry["name"]
        extension = extension_for(entry.get("type"))
        path = target / f"{name}{extension}"
        path.write_text(entry.get("source", ""), encoding="utf-8")
        written_files.append(str(path))
    ok(account_name=account.account_name, script_id=script_id, files=written_files)


def import_dir_to_script(input_data: dict[str, Any]) -> None:
    script_id = str(input_data.get("script_id", "")).strip()
    input_dir = str(input_data.get("input_dir", "")).strip()
    if not script_id or not input_dir:
        fail("script_id and input_dir are required.")
    source_dir = Path(input_dir)
    if not source_dir.is_dir():
        fail(f"input_dir does not exist: {source_dir}")
    files = []
    for path in sorted(source_dir.iterdir()):
        if path.is_file() and path.suffix.lower() in {".gs", ".js", ".html", ".json"}:
            files.append(
                {
                    "name": path.stem,
                    "type": file_type_for(path.suffix.lower()),
                    "source": path.read_text(encoding="utf-8"),
                }
            )
    if not files:
        fail(f"No Apps Script source files found in {source_dir}")
    update_script_content(
        {
            "account_name": input_data.get("account_name"),
            "script_id": script_id,
            "files": files,
        }
    )


def create_script_version(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    script_id = str(input_data.get("script_id", "")).strip()
    if not script_id:
        fail("script_id is required.")
    body = {}
    if input_data.get("description"):
        body["description"] = input_data["description"]
    version = script.projects().versions().create(scriptId=script_id, body=body).execute()
    ok(account_name=account.account_name, script_id=script_id, version=version)


def list_deployments(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    script_id = str(input_data.get("script_id", "")).strip()
    if not script_id:
        fail("script_id is required.")
    deployments = script.projects().deployments().list(scriptId=script_id).execute()
    ok(
        account_name=account.account_name,
        script_id=script_id,
        deployments=deployments.get("deployments", []),
    )


def deploy_script(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    script_id = str(input_data.get("script_id", "")).strip()
    version_number = input_data.get("version_number")
    if not script_id or not version_number:
        fail("script_id and version_number are required.")
    body = {"versionNumber": int(version_number)}
    if input_data.get("description"):
        body["description"] = input_data["description"]
    if input_data.get("manifest_file_name"):
        body["manifestFileName"] = input_data["manifest_file_name"]
    deployment_id = str(input_data.get("deployment_id", "")).strip()
    if deployment_id:
        deployment = script.projects().deployments().update(
            scriptId=script_id,
            deploymentId=deployment_id,
            body=body,
        ).execute()
    else:
        deployment = script.projects().deployments().create(
            scriptId=script_id,
            body=body,
        ).execute()
    ok(account_name=account.account_name, script_id=script_id, deployment=deployment)


def run_script_function(input_data: dict[str, Any]) -> None:
    script, account = build_service("script", "v1", input_data.get("account_name"))
    script_id = str(input_data.get("script_id", "")).strip()
    function_name = str(input_data.get("function", "")).strip()
    if not script_id or not function_name:
        fail("script_id and function are required.")
    body = {
        "function": function_name,
        "parameters": input_data.get("parameters", []),
        "devMode": bool(input_data.get("dev_mode", False)),
    }
    result = script.scripts().run(scriptId=script_id, body=body).execute()
    ok(account_name=account.account_name, script_id=script_id, result=result)


def list_users(input_data: dict[str, Any]) -> None:
    directory, account = build_service("admin", "directory_v1", input_data.get("account_name"))
    result = directory.users().list(
        customer=input_data.get("customer") or "my_customer",
        domain=input_data.get("domain"),
        query=input_data.get("query"),
        maxResults=int(input_data.get("max_results", 100)),
        orderBy="email",
    ).execute()
    ok(account_name=account.account_name, users=result.get("users", []))


def extension_for(file_type: str | None) -> str:
    mapping = {
        "SERVER_JS": ".gs",
        "HTML": ".html",
        "JSON": ".json",
    }
    return mapping.get(file_type or "", ".txt")


def file_type_for(suffix: str) -> str:
    mapping = {
        ".gs": "SERVER_JS",
        ".js": "SERVER_JS",
        ".html": "HTML",
        ".json": "JSON",
    }
    return mapping.get(suffix, "SERVER_JS")


ACTIONS = {
    "configure-account": configure_account,
    "login": login,
    "list-accounts": list_accounts,
    "delete-account": delete_account,
    "list-scripts": list_scripts,
    "create-script-project": create_script_project,
    "get-script-content": get_script_content,
    "update-script-content": update_script_content,
    "export-script-to-dir": export_script_to_dir,
    "import-dir-to-script": import_dir_to_script,
    "create-script-version": create_script_version,
    "list-deployments": list_deployments,
    "deploy-script": deploy_script,
    "run-script-function": run_script_function,
    "list-users": list_users,
}


def main() -> None:
    if len(sys.argv) < 2:
        fail("No action provided.")
    action = sys.argv[1]
    handler = ACTIONS.get(action)
    if handler is None:
        fail(f"Unknown action '{action}'.", available_actions=sorted(ACTIONS))
    input_data = read_input()
    try:
        handler(input_data)
    except Exception as error:  # pragma: no cover - defensive bridge boundary
        fail(str(error), action=action)


if __name__ == "__main__":
    main()
