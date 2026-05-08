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
  await verifyTokenLifecycle();
  const { token } = await createToken(`e2e-${Date.now()}`);
  await writeNpmrc(root, token);
  await publishAndInstallWithNpm(token);
  await publishAndInstallWithPnpm(token);
} finally {
  if (!process.env.E2E_KEEP_TMP) {
    await rm(root, { recursive: true, force: true });
  } else {
    console.log(`kept temp workspace: ${root}`);
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
  await run('npm', ['publish', '--registry', baseUrl, '--tag', 'npm-smoke', '--access', 'restricted'], {
    cwd: packageDir,
    env: authEnv(token)
  });
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
  await run('npm', ['install', `${packageName}@${version}`, '--registry', baseUrl], {
    cwd: consumer,
    env: authEnv(token)
  });
}

async function publishAndInstallWithPnpm(token) {
  const packageName = `${scope}/pnpm-pkg`;
  const packageDir = await createPackage('pnpm-pkg', packageName, token);
  await run('pnpm', ['publish', '--registry', baseUrl, '--tag', 'pnpm-smoke', '--no-git-checks'], {
    cwd: packageDir,
    env: authEnv(token)
  });

  const consumer = path.join(root, 'pnpm-consumer');
  await mkdir(consumer, { recursive: true });
  await writeFile(path.join(consumer, 'package.json'), '{"private":true}\n');
  await writeNpmrc(consumer, token);
  await run('pnpm', ['add', `${packageName}@${version}`, '--registry', baseUrl], {
    cwd: consumer,
    env: authEnv(token)
  });
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
        main: 'index.js',
        files: ['index.js']
      },
      null,
      2
    )}\n`
  );
  await writeFile(path.join(packageDir, 'index.js'), `export const value = '${packageName}@${version}';\n`);
  return packageDir;
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
