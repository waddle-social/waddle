import { expect, test } from "bun:test";
import { stat } from "node:fs/promises";
import { resolve } from "node:path";

const repositoryRoot = resolve(import.meta.dir, "..");
const readSource = (path: string) => Bun.file(resolve(repositoryRoot, path)).text();

test("publishes only verified digest-addressed release artifacts before moving convenience tags", async () => {
  const taskSource = await readSource("server/env.cue");
  const orchestrator = await readSource("server/scripts/publish-waddle-release.sh");
  const imagePublisher = await readSource("server/scripts/publish-container-image.sh");
  const tagPublisher = await readSource("server/scripts/publish-container-tags.sh");
  const chartPublisher = await readSource("server/scripts/publish-helm-chart.sh");
  const extensionPublisher = await readSource("server/scripts/publish-extension-modules.sh");
  const gitopsPublisher = await readSource("server/scripts/publish-gitops-release.sh");
  const chartSource = await readSource(
    "infrastructure/waddle.cloud/gitops/waddle-server/chart-ocirepository.yaml",
  );
  const helmRelease = await readSource(
    "infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml",
  );
  const serverSource = await readSource(
    "infrastructure/waddle.cloud/gitops/waddle-server-source.yaml",
  );

  expect(taskSource).toContain('args: ["scripts/publish-waddle-release.sh"]');
  expect(taskSource).toContain('outputs: ["target/digests/**"]');
  for (const script of [
    "publish-waddle-release.sh",
    "publish-container-image.sh",
    "publish-container-tags.sh",
    "publish-extension-modules.sh",
    "publish-helm-chart.sh",
    "publish-gitops-release.sh",
  ]) {
    expect(taskSource).toContain(`"scripts/${script}"`);
    const mode = (await stat(resolve(repositoryRoot, "server/scripts", script))).mode;
    expect(mode & 0o111).not.toBe(0);
  }
  expect(taskSource).not.toContain("helm push");

  const cleanGuard = orchestrator.indexOf("status --porcelain --untracked-files=normal");
  const registryLogin = orchestrator.indexOf("docker login ghcr.io");
  expect(cleanGuard).toBeGreaterThanOrEqual(0);
  expect(registryLogin).toBeGreaterThan(cleanGuard);

  const stages = [
    orchestrator.indexOf('"${script_dir}/publish-container-image.sh"'),
    orchestrator.indexOf('"${script_dir}/publish-helm-chart.sh"'),
    orchestrator.indexOf('"${script_dir}/publish-extension-modules.sh"'),
    orchestrator.indexOf('"${script_dir}/publish-gitops-release.sh"'),
    orchestrator.indexOf('"${script_dir}/publish-container-tags.sh"'),
  ];
  expect(stages.every((index) => index >= 0)).toBeTrue();
  expect(stages).toEqual([...stages].sort((left, right) => left - right));

  const imagePreflight = imagePublisher.indexOf(
    'docker buildx imagetools inspect "${commit_ref}"',
  );
  const imagePush = imagePublisher.indexOf('docker push "${commit_ref}"');
  const imageDigestPull = imagePublisher.lastIndexOf('docker pull "${repository}@${digest}"');
  expect(imagePreflight).toBeGreaterThanOrEqual(0);
  expect(imagePush).toBeGreaterThan(imagePreflight);
  expect(imageDigestPull).toBeGreaterThan(imagePush);
  expect(imagePublisher).toContain("local_image_id=");
  expect(imagePublisher).toContain("local_rootfs=");
  expect(imagePublisher).toContain("existing_image_id=");
  expect(imagePublisher).toContain("existing_rootfs=");
  expect(imagePublisher).toContain("remote_image_id=");
  expect(imagePublisher).toContain("remote_rootfs=");
  expect(imagePublisher).toContain("not found|manifest unknown|manifest_unknown|404");
  expect(imagePublisher).not.toContain("RepoDigests");

  expect(tagPublisher).toContain("tag_args=()");
  expect(tagPublisher).toContain("imagetools create --prefer-index=false");
  expect(tagPublisher).not.toContain(
    'tag_args=("-t" "${repository}:sha-${FULL_SHA}")',
  );
  expect(tagPublisher).toContain('commit_ref="${repository}:sha-${FULL_SHA}"');
  expect(tagPublisher).toContain('final_commit_ref_digest="$(remote_digest "${commit_ref}")"');

  const preflight = chartPublisher.indexOf(
    'if helm pull "${chart_url}" --version "${chart_version}" --untar',
  );
  const push = chartPublisher.indexOf(
    'helm push "${chart_package}" oci://ghcr.io/waddle-social/waddle/charts',
  );
  const resolveDigest = chartPublisher.indexOf(
    'chart_digest="$(oras resolve "${chart_ref}:${chart_version}")"',
  );
  const digestPull = chartPublisher.indexOf(
    'helm pull "${chart_url}@${chart_digest}" --untar',
  );
  const finalDigestCheck = gitopsPublisher.indexOf(
    'current_chart_digest="$(oras resolve "${chart_ref}:${chart_version}")"',
  );
  const gitopsPush = gitopsPublisher.indexOf(
    'flux push artifact "oci://${commit_gitops_ref}"',
  );

  for (const index of [preflight, push, resolveDigest, digestPull, finalDigestCheck, gitopsPush]) {
    expect(index).toBeGreaterThanOrEqual(0);
  }
  expect(preflight).toBeLessThan(push);
  expect(push).toBeLessThan(resolveDigest);
  expect(resolveDigest).toBeLessThan(digestPull);
  expect(finalDigestCheck).toBeLessThan(gitopsPush);

  expect(chartPublisher).toContain(
    'diff -qr "${work_dir}/local/waddle-server" "${work_dir}/remote-preflight/waddle-server"',
  );
  expect(chartPublisher).toContain(
    'diff -qr "${work_dir}/local/waddle-server" "${work_dir}/remote-verified/waddle-server"',
  );
  expect(gitopsPublisher).toContain(
    'if [[ "${current_chart_digest}" != "${chart_digest}" ]]; then',
  );
  expect(chartPublisher).toContain(
    'printf \'%s\\n\' "${chart_digest}" > "${RELEASE_DIGEST_DIR}/waddle-server-chart-${chart_version}.txt"',
  );

  const extensionPreflight = extensionPublisher.indexOf(
    'oras resolve "${extension_ref}" 2>"${resolve_log}"',
  );
  const extensionPush = extensionPublisher.indexOf("oras push \\");
  const extensionPull = extensionPublisher.indexOf(
    'oras pull "${extension_repository}@${extension_digest}"',
  );
  expect(extensionPreflight).toBeGreaterThanOrEqual(0);
  expect(extensionPush).toBeGreaterThan(extensionPreflight);
  expect(extensionPull).toBeGreaterThan(extensionPush);
  expect(extensionPublisher).toContain('remote_entries=("${remote_dir}"/*)');
  expect(extensionPublisher).toContain('"${#remote_entries[@]}" -ne 1');
  expect(extensionPublisher).toContain('[[ -L "${remote_dir}/module.wasm" ]]');
  expect(extensionPublisher).toContain(
    'cmp -s "${local_dir}/module.wasm" "${remote_dir}/module.wasm"',
  );

  expect(chartSource).toContain("kind: OCIRepository");
  expect(chartSource).toContain("name: waddle-server-chart");
  expect(chartSource).toContain("tag: 0.5.0");
  expect(chartSource).toContain("application/vnd.cncf.helm.chart.content.v1.tar+gzip");
  expect(helmRelease).toContain("chartRef:");
  expect(helmRelease).toContain("name: waddle-server-chart");
  expect(helmRelease).not.toContain("kind: HelmRepository");
  expect(gitopsPublisher).toContain('.spec.ref = {"digest": strenv(CHART_DIGEST)}');
  expect(gitopsPublisher).toContain("kubectl kustomize");

  const immutableGitopsPush = gitopsPublisher.indexOf(
    'flux push artifact "oci://${commit_gitops_ref}"',
  );
  const immutableGitopsPull = gitopsPublisher.lastIndexOf(
    'flux pull artifact "oci://${gitops_repository}@${gitops_digest}"',
  );
  const latestPromotion = gitopsPublisher.indexOf(
    'oras cp "${gitops_repository}@${gitops_digest}" "${gitops_repository}:latest"',
  );
  expect(gitopsPublisher).toContain('commit_gitops_ref="${gitops_repository}:sha-${FULL_SHA}"');
  expect(gitopsPublisher).toContain("--reproducible");
  expect(gitopsPublisher).toContain('--revision="${GITHUB_REF_NAME}@sha1:${FULL_SHA}"');
  expect(gitopsPublisher).toContain(
    'printf \'%s\\n\' "${gitops_digest}" > "${RELEASE_DIGEST_DIR}/waddle-server-gitops-digest.txt"',
  );
  expect(immutableGitopsPull).toBeGreaterThan(immutableGitopsPush);
  expect(latestPromotion).toBeGreaterThan(immutableGitopsPull);
  expect(gitopsPublisher).not.toContain(
    "flux push artifact oci://ghcr.io/waddle-social/waddle/gitops-waddle-server:latest",
  );

  // This is an explicit remaining bootstrap boundary, not an immutability claim:
  // the root GitOps bundle owns this source and still discovers the verified
  // full-SHA artifact through the promoted latest tag.
  expect(serverSource).toContain("tag: latest");
  expect(gitopsPublisher).toContain("needs a separate ownership");

  expect([
    taskSource,
    orchestrator,
    imagePublisher,
    chartPublisher,
    extensionPublisher,
    gitopsPublisher,
    tagPublisher,
  ].join("\n")).not.toContain("SHORT_SHA");
  expect(imagePublisher).toContain(
    'commit_ref="${repository}:sha-${FULL_SHA}"',
  );
  expect(extensionPublisher).toContain(
    'extension_ref="ghcr.io/waddle-social/waddle/extensions/${extension_name}:sha-${FULL_SHA}"',
  );
  expect(gitopsPublisher).toContain('--revision="${GITHUB_REF_NAME}@sha1:${FULL_SHA}"');
});

test("keeps Gate 0 tasks declarative and their environment contracts explicit", async () => {
  const taskSource = await readSource("server/env.cue");
  const collector = await readSource("server/scripts/collect-capability-baseline.sh");
  const finalizer = await readSource("server/scripts/finalize-gate-zero-baseline.sh");

  expect(taskSource).toContain('args: ["scripts/collect-capability-baseline.sh"]');
  expect(taskSource).toContain("finalizeGateZeroBaseline: schema.#Task");
  expect(taskSource).toContain('args: ["scripts/finalize-gate-zero-baseline.sh"]');
  expect(taskSource).not.toContain("finalizeCapabilityBaseline");
  expect(taskSource).not.toContain("finalizeTelemetryBaseline");
  expect(taskSource).toContain("env: _capabilityCollectionEnv");
  expect(taskSource.match(/env: _baselineFinalizationEnv/g)).toHaveLength(1);
  expect(taskSource).not.toContain("collector_args=(");
  expect(taskSource.split("\n").length).toBeLessThan(1_000);

  for (const required of [
    "WADDLE_CAPABILITY_ENDPOINT",
    "WADDLE_CAPABILITY_ACCOUNT_JID",
    "WADDLE_CAPABILITY_ACCESS_TOKEN",
    "WADDLE_CAPABILITY_SERVER_COMMIT",
    "WADDLE_CAPABILITY_WINDOW_START",
    "WADDLE_CAPABILITY_WINDOW_END",
  ]) {
    expect(taskSource).toContain(required + ":");
    expect(collector).toContain("${" + required + ":?");
  }
  expect(finalizer).toContain(
    'bun "${repo_root}/scripts/finalize-switchable-baseline.ts" all',
  );
  for (const stagedInput of [
    "capability/live-disco-export.json",
    "prometheus/telemetry-baseline.json",
    "faro/browser-auth-bootstrap.json",
    "faro/browser-message-ack-latency.json",
    "faro/browser-session-lifecycle.json",
    "faro/browser-reconnect-duration.json",
    "attestation/live-collection-subject.json",
    "attestation/live-collection.sigstore.json",
  ]) {
    expect(finalizer).toContain(`\${staging_root}/${stagedInput}`);
    expect(taskSource).toContain(stagedInput);
  }
  expect(finalizer).toContain("--identity-metric waddle_build_info");
  expect(finalizer).toContain("--target-signal-id server-deployment-identity-targets");
  expect(finalizer).toContain("--identity-lookback-seconds 3600");
  expect(taskSource).toContain('outputs: ["../docs/evidence/gate-0/**"]');
  const finalizerMode = (
    await stat(
      resolve(repositoryRoot, "server/scripts/finalize-gate-zero-baseline.sh"),
    )
  ).mode;
  expect(finalizerMode & 0o111).not.toBe(0);
  expect(
    await Bun.file(
      resolve(repositoryRoot, "server/scripts/finalize-capability-baseline.sh"),
    ).exists(),
  ).toBeFalse();
  expect(
    await Bun.file(
      resolve(repositoryRoot, "server/scripts/finalize-telemetry-baseline.sh"),
    ).exists(),
  ).toBeFalse();
});

test("includes the capability contract in every Nix server check source", async () => {
  const source = await Bun.file(resolve(repositoryRoot, "flake.nix")).text();
  expect(source.match(/\.\/server\/disco-target-contract\.json/g)).toHaveLength(2);
});
