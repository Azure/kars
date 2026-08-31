// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

'use strict';

const fs = require('fs');
const path = require('path');

function sanitizeFeishuAxiosError(error) {
	const sanitized = new Error('Feishu request failed');
	sanitized.name = 'FeishuRequestError';
	sanitized.isAxiosError = true;
	if (typeof error?.code === 'string' && /^[A-Z0-9_-]{1,64}$/.test(error.code)) {
		sanitized.code = error.code;
	}
	if (Number.isInteger(error?.response?.status) && error.response.status >= 100 && error.response.status <= 599) {
		sanitized.status = error.response.status;
	}
	return sanitized;
}

function main() {
const stageDir = process.argv[2] || '/opt/openclaw-feishu-stage';
const distDir = path.join(stageDir, 'npm', 'node_modules', '@openclaw', 'feishu', 'dist');
const candidates = fs.readdirSync(distDir).filter((name) => /^client-.*\.js$/.test(name));
if (candidates.length !== 1) {
  throw new Error(`expected one Feishu client bundle, found ${candidates.length}`);
}

const bundle = path.join(distDir, candidates[0]);
let source = fs.readFileSync(bundle, 'utf8');
const oldBlock = `\tconst agent = await getWsProxyAgent();
\treturn new feishuClientSdk.WSClient({
\t\tappId,
\t\tappSecret,
\t\tdomain: resolveDomain(domain),
\t\t...callbacks,`;
const newBlock = `\tconst agent = await getWsProxyAgent();
\tconst httpInstance = agent ? {
\t\trequest: (opts) => feishuClientSdk.defaultHttpInstance.request({
\t\t\t...opts,
\t\t\tproxy: false,
\t\t\thttpAgent: agent,
\t\t\thttpsAgent: agent
\t\t})
\t} : feishuClientSdk.defaultHttpInstance;
\treturn new feishuClientSdk.WSClient({
\t\tappId,
\t\tappSecret,
\t\tdomain: resolveDomain(domain),
\t\thttpInstance,
\t\t...callbacks,`;

const occurrences = source.split(oldBlock).length - 1;
if (occurrences === 1) {
	source = source.replace(oldBlock, newBlock);
} else if (!(occurrences === 0 && source.includes('httpsAgent: agent'))) {
  throw new Error(`expected one Feishu WS client source anchor, found ${occurrences}`);
}

const oldInterceptor = `\t\tinst.interceptors.request.use((req) => {
			const r = req;
			if (r.headers) r.headers["User-Agent"] = getFeishuUserAgent();
			return req;
		});`;
const newInterceptor = `${sanitizeFeishuAxiosError.toString()}
		inst.interceptors.request.use(async (req) => {
			const r = req;
			if (r.headers) r.headers["User-Agent"] = getFeishuUserAgent();
			const agent = await getWsProxyAgent();
			if (agent) {
				r.proxy = false;
				r.httpAgent = agent;
				r.httpsAgent = agent;
			}
			return req;
		});
		inst.interceptors.response?.use(undefined, (error) => {
			return Promise.reject(sanitizeFeishuAxiosError(error));
		});`;
const interceptorOccurrences = source.split(oldInterceptor).length - 1;
if (interceptorOccurrences === 1) {
  source = source.replace(oldInterceptor, newInterceptor);
} else if (!(interceptorOccurrences === 0 && source.includes('sanitizeFeishuAxiosError(error)'))) {
  throw new Error(`expected one Feishu Axios interceptor anchor, found ${interceptorOccurrences}`);
}
if (!source.includes('sanitizeFeishuAxiosError(error)')) {
	throw new Error('Feishu Axios error redaction patch missing');
}
fs.writeFileSync(bundle, source);
console.log(`Patched Feishu Axios proxy transport: ${bundle}`);
}

if (require.main === module) {
	main();
}

module.exports = { sanitizeFeishuAxiosError };
