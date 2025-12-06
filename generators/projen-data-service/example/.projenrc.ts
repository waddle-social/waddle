import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { WaddleDataService } from "../src/index";

const __dirname = dirname(fileURLToPath(import.meta.url));
process.chdir(__dirname);

// Example: Basic data service
const service = new WaddleDataService({
	serviceName: "example-service",
	bindings: {
		d1Databases: [
			{
				binding: "DB",
				database_name: "waddle-example",
				database_id: "YOUR-D1-DATABASE-ID-HERE",
			},
		],
		// Optional: Add other bindings as needed
		// kvNamespaces: [{ binding: "KV", id: "YOUR-KV-ID" }],
		// r2Buckets: [{ binding: "BUCKET", bucket_name: "my-bucket" }],
		// services: [{ binding: "OTHER_SERVICE", service: "other-service" }],
	},
	includeWriteModel: false,
});

service.synth();
