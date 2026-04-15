from __future__ import annotations

import platform
import sys
from dataclasses import dataclass
from pathlib import Path

from .deferred_init import DeferredInitResult, run_deferred_init
from .prefetch import PrefetchResult, start_keychain_prefetch, start_mdm_raw_read, start_project_scan


@dataclass(frozen=True)
class PlatformProfile:
    raw_name: str
    family: str
    preferred_shell_tools: tuple[str, ...]
    preferred_shell_commands: tuple[str, ...]
    shell_guidance: str


@dataclass(frozen=True)
class WorkspaceSetup:
    python_version: str
    implementation: str
    platform_name: str
    platform_profile: PlatformProfile
    test_command: str = 'python3 -m unittest discover -s tests -v'

    def startup_steps(self) -> tuple[str, ...]:
        return (
            'start top-level prefetch side effects',
            'build workspace context',
            'load mirrored command snapshot',
            'load mirrored tool snapshot',
            'prepare parity audit hooks',
            'apply trust-gated deferred init',
        )


@dataclass(frozen=True)
class SetupReport:
    setup: WorkspaceSetup
    prefetches: tuple[PrefetchResult, ...]
    deferred_init: DeferredInitResult
    trusted: bool
    cwd: Path

    def as_markdown(self) -> str:
        lines = [
            '# Setup Report',
            '',
            f'- Python: {self.setup.python_version} ({self.setup.implementation})',
            f'- Platform: {self.setup.platform_name}',
            f'- Shell policy: {self.setup.platform_profile.shell_guidance}',
            f'- Trusted mode: {self.trusted}',
            f'- CWD: {self.cwd}',
            '',
            'Prefetches:',
            *(f'- {prefetch.name}: {prefetch.detail}' for prefetch in self.prefetches),
            '',
            'Deferred init:',
            *self.deferred_init.as_lines(),
        ]
        return '\n'.join(lines)


def build_workspace_setup(platform_name: str | None = None) -> WorkspaceSetup:
    resolved_platform_name = platform_name or platform.platform()
    return WorkspaceSetup(
        python_version='.'.join(str(part) for part in sys.version_info[:3]),
        implementation=platform.python_implementation(),
        platform_name=resolved_platform_name,
        platform_profile=build_platform_profile(resolved_platform_name),
    )


def build_platform_profile(platform_name: str) -> PlatformProfile:
    lowered = platform_name.lower()
    if 'windows' in lowered or sys.platform.startswith('win'):
        return PlatformProfile(
            raw_name=platform_name,
            family='windows',
            preferred_shell_tools=('PowerShellTool',),
            preferred_shell_commands=('terminalSetup',),
            shell_guidance='Windows detected: prefer PowerShellTool and terminalSetup; avoid BashTool unless the user explicitly asks for Bash or WSL.',
        )

    return PlatformProfile(
        raw_name=platform_name,
        family='posix',
        preferred_shell_tools=('BashTool',),
        preferred_shell_commands=(),
        shell_guidance='Non-Windows host: BashTool remains the default shell surface.',
    )


def run_setup(cwd: Path | None = None, trusted: bool = True, platform_name: str | None = None) -> SetupReport:
    root = cwd or Path(__file__).resolve().parent.parent
    prefetches = [
        start_mdm_raw_read(),
        start_keychain_prefetch(),
        start_project_scan(root),
    ]
    setup = build_workspace_setup(platform_name=platform_name)
    return SetupReport(
        setup=setup,
        prefetches=tuple(prefetches),
        deferred_init=run_deferred_init(trusted=trusted),
        trusted=trusted,
        cwd=root,
    )
