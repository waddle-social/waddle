import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { WaddleDataService } from "../../../generators/projen-data-service/src/index";

const __dirname = dirname(fileURLToPath(import.meta.url));
process.chdir(__dirname);

const service = new WaddleDataService({
	serviceName: "topics",
	bindings: {
		d1Databases: [
			{
				binding: "DB",
				database_name: "waddle-service-topics",
				database_id: "a4aea118-e1e2-44ff-83e6-2ffb6bd18c28",
			},
		],
	},
	includeWriteModel: false,
});

service.synth();
