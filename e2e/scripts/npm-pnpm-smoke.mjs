import { mkdtemp, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';

const baseUrl = process.env.E2E_BASE_URL || 'http://localhost:3333';
const bootstrapToken = process.env.E2E_BOOTSTRAP_ADMIN_TOKEN || 'dev-bootstrap-admin-token';
const scope = '@mini-smoke';
const version = `0.0.0-smoke.${Date.now()}`;

const root = await mkdtemp(path.join(tmpdir(), 'mini-packages-e2e-'));

try {
  await verifyPing();
  await verifyTokenLifecycle();
  const { token } = await createToken(`e2e-${Date.now()}`);
  await writeNpmrc(root, token);
  await publishAndInstallWithNpm(token);
  await publishAndInstallWithPnpm(token);
  await verifyLocalLinkWorkflows(token);
} finally {
  if (!process.env.E2E_KEEP_TMP) {
    await rm(root, { recursive: true, force: true });
  } else {
    console.log(`kept temp workspace: ${root}`);
  }
}

async function verifyPing() {
  const response = await fetch(`${baseUrl}/-/ping`);
  if (!response.ok) {
    throw new Error(`ping failed: ${response.status} ${await response.text()}`);
  }
}

async function verifyTokenLifecycle() {
  const created = await createToken(`lifecycle-${Date.now()}`);
  await expectWhoami(created.token, 200);

  const rotatedResponse = await fetch(`${baseUrl}/api/v1/tokens/${created.summary.id}/rotate`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${bootstrapToken}`
    }
  });
  if (!rotatedResponse.ok) {
    throw new Error(`token rotate failed: ${rotatedResponse.status} ${await rotatedResponse.text()}`);
  }
  const rotated = await rotatedResponse.json();
  await expectWhoami(created.token, 401);
  await expectWhoami(rotated.token, 200);

  const revokeResponse = await fetch(`${baseUrl}/api/v1/tokens/${created.summary.id}/revoke`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${bootstrapToken}`
    }
  });
  if (!revokeResponse.ok) {
    throw new Error(`token revoke failed: ${revokeResponse.status} ${await revokeResponse.text()}`);
  }
  await expectWhoami(rotated.token, 401);
}

async function expectWhoami(token, status) {
  const response = await fetch(`${baseUrl}/-/whoami`, {
    headers: {
      authorization: `Bearer ${token}`
    }
  });
  if (response.status !== status) {
    throw new Error(`whoami expected ${status}, got ${response.status}: ${await response.text()}`);
  }
}

async function createToken(name) {
  const response = await fetch(`${baseUrl}/api/v1/tokens`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${bootstrapToken}`,
      'content-type': 'application/json'
    },
    body: JSON.stringify({
      name,
      claims: {
        read: [`${scope}/*`],
        publish: [`${scope}/*`]
      }
    })
  });
  if (!response.ok) {
    throw new Error(`token create failed: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

async function writeNpmrc(dir, token) {
  const registry = baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
  const url = new URL(registry);
  const authPath = url.pathname.endsWith('/') ? url.pathname : `${url.pathname}/`;
  const npmrc = [
    `${scope}:registry=${registry}`,
    `//${url.host}${authPath}:_authToken=${token}`
  ].join('\n');
  await writeFile(path.join(dir, '.npmrc'), `${npmrc}\n`);
}

async function publishAndInstallWithNpm(token) {
  const packageName = `${scope}/npm-pkg`;
  const packageDir = await createPackage('npm-pkg', packageName, token);
  await run('npm', ['publish', '--registry', baseUrl, '--tag', 'beta', '--access', 'restricted'], {
    cwd: packageDir,
    env: authEnv(token)
  });
  await expectNpmDistTag(packageName, 'beta', version, token);
  await run('npm', ['dist-tag', 'add', `${packageName}@${version}`, 'latest', '--registry', baseUrl], {
    cwd: packageDir,
    env: authEnv(token)
  });
  await expectNpmDistTag(packageName, 'latest', version, token);
  await run('npm', ['dist-tag', 'ls', packageName, '--registry', baseUrl], {
    cwd: packageDir,
    env: authEnv(token)
  });
  await run('npm', ['dist-tag', 'add', `${packageName}@${version}`, 'smoke_extra', '--registry', baseUrl], {
    cwd: packageDir,
    env: authEnv(token)
  });
  await run('npm', ['dist-tag', 'rm', packageName, 'smoke_extra', '--registry', baseUrl], {
    cwd: packageDir,
    env: authEnv(token)
  });

  const consumer = path.join(root, 'npm-consumer');
  await mkdir(consumer, { recursive: true });
  await writeFile(path.join(consumer, 'package.json'), '{"private":true}\n');
  await writeNpmrc(consumer, token);
  await run('npm', ['install', `${packageName}@beta`, '--registry', baseUrl], {
    cwd: consumer,
    env: authEnv(token)
  });
  await run('npm', ['install', `${packageName}@latest`, '--registry', baseUrl], {
    cwd: consumer,
    env: authEnv(token)
  });
}

async function publishAndInstallWithPnpm(token) {
  const packageName = `${scope}/pnpm-pkg`;
  const packageDir = await createPackage('pnpm-pkg', packageName, token);
  await run('pnpm', ['publish', '--registry', baseUrl, '--tag', 'beta', '--no-git-checks'], {
    cwd: packageDir,
    env: authEnv(token)
  });
  await expectNpmDistTag(packageName, 'beta', version, token);

  const consumer = path.join(root, 'pnpm-consumer');
  await mkdir(consumer, { recursive: true });
  await writeFile(path.join(consumer, 'package.json'), '{"private":true}\n');
  await writeNpmrc(consumer, token);
  await run('pnpm', ['add', `${packageName}@beta`, '--registry', baseUrl], {
    cwd: consumer,
    env: authEnv(token)
  });
}

async function verifyLocalLinkWorkflows(token) {
  const npmPackageName = `${scope}/npm-link-pkg`;
  const npmPackageDir = await createPackage('npm-link-pkg', npmPackageName, token);
  const npmConsumer = path.join(root, 'npm-link-consumer');
  await mkdir(npmConsumer, { recursive: true });
  await writeFile(path.join(npmConsumer, 'package.json'), '{"private":true,"type":"module"}\n');
  await run('npm', ['link', npmPackageDir], {
    cwd: npmConsumer,
    env: authEnv(token)
  });
  await expectImportValue(npmConsumer, npmPackageName, `${npmPackageName}@${version}`, token);

  const pnpmPackageName = `${scope}/pnpm-link-pkg`;
  const pnpmPackageDir = await createPackage('pnpm-link-pkg', pnpmPackageName, token);
  const pnpmConsumer = path.join(root, 'pnpm-link-consumer');
  await mkdir(pnpmConsumer, { recursive: true });
  await writeFile(path.join(pnpmConsumer, 'package.json'), '{"private":true,"type":"module"}\n');
  await run('pnpm', ['add', `link:${pnpmPackageDir}`], {
    cwd: pnpmConsumer,
    env: authEnv(token)
  });
  await expectImportValue(pnpmConsumer, pnpmPackageName, `${pnpmPackageName}@${version}`, token);
}

async function createPackage(dirname, packageName, token) {
  const packageDir = path.join(root, dirname);
  await mkdir(packageDir, { recursive: true });
  await writeNpmrc(packageDir, token);
  await writeFile(
    path.join(packageDir, 'package.json'),
    `${JSON.stringify(
      {
        name: packageName,
        version,
        description: 'mini packages smoke package',
        type: 'module',
        main: 'index.js',
        exports: './index.js',
        files: ['index.js']
      },
      null,
      2
    )}\n`
  );
  await writeFile(path.join(packageDir, 'index.js'), `export const value = '${packageName}@${version}';\n`);
  return packageDir;
}

async function expectNpmDistTag(packageName, tag, expected, token) {
  const stdout = await runCapture('npm', ['dist-tag', 'ls', packageName, '--registry', baseUrl], {
    cwd: root,
    env: authEnv(token)
  });
  const actual = Object.fromEntries(
    stdout
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => line.split(/:\s+/, 2))
  )[tag];
  if (actual !== expected) {
    throw new Error(`${packageName} dist-tag ${tag} expected ${expected}, got ${actual}`);
  }
}

async function expectImportValue(cwd, packageName, expected, token) {
  const script = [
    `const mod = await import(${JSON.stringify(packageName)});`,
    `if (mod.value !== ${JSON.stringify(expected)}) {`,
    `  throw new Error(${JSON.stringify(`unexpected linked value for ${packageName}`)} + ': ' + mod.value);`,
    '}'
  ].join('\n');
  await run('node', ['--input-type=module', '-e', script], {
    cwd,
    env: authEnv(token)
  });
}

function authEnv(token) {
  void token;
  return {
    PATH: process.env.PATH,
    HOME: root,
    USER: process.env.USER,
    TMPDIR: process.env.TMPDIR,
    TEMP: process.env.TEMP,
    TMP: process.env.TMP,
    COREPACK_HOME: process.env.COREPACK_HOME,
    PNPM_HOME: process.env.PNPM_HOME,
    CI: '1',
    NPM_CONFIG_USERCONFIG: path.join(root, '.npmrc'),
    npm_config_userconfig: path.join(root, '.npmrc'),
    npm_config_registry: baseUrl
  };
}

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: 'inherit',
      cwd: options.cwd || root,
      env: options.env || process.env
    });
    child.on('exit', (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${command} ${args.join(' ')} failed with exit ${code}`));
      }
    });
  });
}

function runCapture(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      cwd: options.cwd || root,
      env: options.env || process.env
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
    });
    child.on('exit', (code) => {
      if (code === 0) {
        resolve(stdout.trim());
      } else {
        reject(new Error(`${command} ${args.join(' ')} failed with exit ${code}\n${stderr || stdout}`));
      }
    });
  });
}
