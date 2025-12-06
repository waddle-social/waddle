import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { WaddleDataService } from "../../../generators/projen-data-service/src/index";

const __dirname = dirname(fileURLToPath(import.meta.url));
process.chdir(__dirname);

const service = new WaddleDataService({
	serviceName: "waddle",
	bindings: {
		d1Databases: [
			{
				binding: "DB",
				database_name: "waddle-waddle",
				database_id: "6c09af2c-2665-4a56-a805-09c40b5e0711",
			},
		],
	},
	includeWriteModel: false,
});

service.synth();
