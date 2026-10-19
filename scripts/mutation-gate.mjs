import { readFileSync } from "node:fs";

const reportPath = "reports/mutation/mutation.json";
const report = JSON.parse(readFileSync(reportPath, "utf8"));
const counts = { Killed: 0, Timeout: 0, Survived: 0, NoCoverage: 0, RuntimeError: 0, CompileError: 0, Ignored: 0, Pending: 0 };

const mutants = Object.values(report.files).flatMap((file) => file.mutants);
for (const mutant of mutants) {
  counts[mutant.status] = (counts[mutant.status] ?? 0) + 1;
}

const detected = counts.Killed + counts.Timeout;
const undetected = counts.Survived + counts.NoCoverage;
const score = mutants.length === 0 ? 100 : (detected / mutants.length) * 100;

console.log(
  `mutation score ${score.toFixed(2)}% (${detected}/${mutants.length}) ` +
    `killed=${counts.Killed} timeout=${counts.Timeout} survived=${counts.Survived} no-coverage=${counts.NoCoverage}`,
);

if (undetected > 0 || counts.RuntimeError > 0 || counts.CompileError > 0 || score !== 100) {
  for (const mutant of mutants) {
    if (mutant.status !== "Killed" && mutant.status !== "Timeout" && mutant.status !== "Ignored") {
      console.error(`${mutant.id} ${mutant.status} ${mutant.mutatorName} ${mutant.location.start.line}:${mutant.location.start.column} ${mutant.replacement ?? ""}`);
    }
  }
  process.exit(1);
}
