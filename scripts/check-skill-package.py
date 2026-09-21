#!/usr/bin/env python3
"""Verify skill delivery through real Cargo archives, installation and a moved binary.

Run after the normal Cargo build has populated the dependency cache. Packaging
runs offline; installation also uses the packaged lockfile. Local dependency
patches refer only to extracted .crate archives, so unreleased workspace changes
need not be published first.
No original source path supplies guide content to the installed binary.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile


def run(*args, cwd=None, capture=False, env=None):
    result = subprocess.run(args, cwd=cwd, env=env, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None)
    return result.stdout if capture else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--allow-dirty', action='store_true')
    parser.add_argument('--output', type=Path, help='Optional JSON verification receipt')
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(run('cargo', 'metadata', '--no-deps', '--format-version', '1',
                              cwd=root, capture=True))
    packages = {p['name']: p for p in metadata['packages'] if p['source'] is None}
    selected = set()

    def visit(name):
        if name in selected:
            return
        selected.add(name)
        for dependency in packages[name]['dependencies']:
            if dependency['name'] in packages and dependency['kind'] != 'dev':
                visit(dependency['name'])

    visit('nits')
    command = ['cargo', 'package', '--no-verify', '--offline']
    if args.allow_dirty:
        command.append('--allow-dirty')
    for name in sorted(selected):
        command.extend(['-p', name])
    run(*command, cwd=root)

    with tempfile.TemporaryDirectory(prefix='nits-skill-package-') as directory:
        temporary = Path(directory)
        extracted = {}
        for name in sorted(selected):
            version = packages[name]['version']
            archive = Path(metadata['target_directory']) / 'package' / f'{name}-{version}.crate'
            with tarfile.open(archive) as tar:
                tar.extractall(temporary / 'source', filter='data')
            extracted[name] = temporary / 'source' / f'{name}-{version}'
        # Cargo must flatten the source symlink into real archive members.
        for relative in ['SKILL.md', 'references/interaction.md', 'references/revisions.md']:
            packaged = extracted['nits'] / 'bundled-skill' / relative
            assert packaged.is_file() and not packaged.is_symlink(), relative
            assert packaged.read_bytes() == (root / 'skills/nits-review' / relative).read_bytes(), relative
        config = temporary / 'dependencies.toml'
        config.write_text('[patch.crates-io]\n' + ''.join(
            f'{json.dumps(name)} = {{ path = {json.dumps(str(path))} }}\n'
            for name, path in sorted(extracted.items()) if name != 'nits'))
        install = temporary / 'installed'
        # Match the archived dependency resolution, including locked versions
        # that have since been yanked. Re-resolving from a CI cache can fail or
        # select dependencies that were never built by the preceding checks.
        run('cargo', 'install', '--locked', '--offline', '--debug', '--path', str(extracted['nits']),
            '--root', str(install), '--config', str(config), cwd=temporary)
        binary = install / 'bin/nits'
        offline = temporary / 'offline'
        offline.mkdir()
        broken = offline / 'broken.toml'
        broken.write_text('not = [valid toml')
        env = {name: value for name, value in os.environ.items() if not name.startswith('NITS_')}
        env.update(NITS_CONFIG=str(broken), NITS_CONTEXT='missing-context',
                   XDG_DATA_HOME=str(offline / 'data'), XDG_CONFIG_HOME=str(offline / 'config'))
        installed = run(str(binary), 'skill', cwd=offline, env=env, capture=True)
        # Remove every extracted source and move the executable, matching a
        # standalone release archive install. Content must remain identical.
        moved = offline / 'nits-standalone'
        shutil.copy2(binary, moved)
        shutil.rmtree(temporary / 'source')
        standalone = run(str(moved), 'skill', cwd=offline, env=env, capture=True)
        assert installed == standalone
        assert installed.startswith('---\nname: nits-review\n')
        for heading in ['# MCP and CLI interaction', '# Revisions, findings and handoff references']:
            assert heading in installed, heading
        assert '(references/' not in installed
        assert not (offline / 'data').exists() and not (offline / 'config').exists()
        receipt = {'packages': sorted(selected), 'cargo_install': True, 'locked': True,
                   'standalone': True,
                   'canonical_files': 3, 'markdown_bytes': len(installed.encode()),
                   'source_removed_before_standalone': True}
        if args.output:
            args.output.write_text(json.dumps(receipt, indent=2) + '\n')
        print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
