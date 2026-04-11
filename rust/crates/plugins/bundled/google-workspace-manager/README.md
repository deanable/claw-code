# Google Workspace Manager

Bundled Claw plugin for managing Google Workspace accounts and Google Apps Script projects.

This plugin is designed as a practical bridge between Claw and the Google APIs you need for Apps Script work:

- authenticate multiple Google accounts
- save account profiles locally
- list Apps Script projects
- create projects
- pull script content into a workspace folder
- push edited files back to Apps Script
- create versions
- create and update deployments
- run Apps Script API executable functions
- perform basic Google Workspace Admin SDK user listing

## Current shape

The plugin ships as a bundled plugin plus a Python bridge script:

- manifest: `.claude-plugin/plugin.json`
- bridge: `tools/google_workspace_bridge.py`

The bridge stores account and token data outside the repo in a user data directory:

- Windows: `%APPDATA%\\claw\\plugins\\google-workspace-manager`
- macOS/Linux: `~/.claw/plugins/google-workspace-manager`

## Prerequisites

1. Install Python 3.
2. Install the Google client libraries listed in `tools/google_workspace_requirements.txt`.
3. Create a Google Cloud OAuth desktop client and download the client secret JSON.
4. Enable the APIs you plan to use:
   - Apps Script API
   - Drive API
   - Admin SDK API

## Typical flow

1. Configure an account with `google_workspace_configure_account`.
2. Run `google_workspace_login` for that account.
3. List projects with `google_workspace_list_scripts`.
4. Export a script into a workspace folder with `google_workspace_export_script_to_dir`.
5. Edit files locally.
6. Push them back with `google_workspace_import_dir_to_script`.
7. Version and deploy with:
   - `google_workspace_create_script_version`
   - `google_workspace_deploy_script`

## Notes

- The plugin favors the Google APIs directly instead of relying on `clasp`, which makes multi-account flows easier to reason about.
- Admin SDK actions require an account with the right Workspace admin privileges and scopes.
- This is an initial implementation focused on Apps Script and account management. It is meant to be a strong foundation rather than the final surface area.
