from __future__ import annotations

from .commands import built_in_command_names, get_commands
from .setup import run_setup
from .tools import get_tools


def build_system_init_message(trusted: bool = True, platform_name: str | None = None) -> str:
    setup = run_setup(trusted=trusted, platform_name=platform_name)
    active_platform = platform_name or setup.setup.platform_name
    commands = get_commands()
    tools = get_tools(platform_name=active_platform)
    profile = setup.setup.platform_profile
    lines = [
        '# System Init',
        '',
        f'Trusted: {setup.trusted}',
        f'Platform: {profile.raw_name}',
        f'Shell policy: {profile.shell_guidance}',
        f'Built-in command names: {len(built_in_command_names())}',
        f'Loaded command entries: {len(commands)}',
        f'Loaded tool entries: {len(tools)}',
        '',
        'Startup steps:',
        *(f'- {step}' for step in setup.setup.startup_steps()),
    ]
    return '\n'.join(lines)
