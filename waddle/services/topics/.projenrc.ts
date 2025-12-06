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
				database_name: "waddle-topics",
				database_id: "TODO-REPLACE-WITH-TOPICS-D1-ID",
			},
		],
	},
	includeWriteModel: false,
});

service.synth();
