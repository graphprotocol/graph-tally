# Grafana

- `dashboards/`: dashboard exports. Import them in Grafana with **Dashboards → New → Import**.
- `alerts/`: alert rules in Prometheus rule format.

## Importing alert rules

The rules become Grafana-managed rules. Their `team` label routes them to Rootly through the
notification policies in graph-infra (`infra/terraform/grafana-cloud`).

1. Go to **Alerting → Alert rules → Import to Grafana-managed rules**.
2. Select **Prometheus YAML file** and upload the file, for example `alerts/escrow_manager.yml`.
3. Select the VictoriaMetrics data source that the dashboards use.
4. In **Additional settings**, select a folder that holds only these rules, for example
   `Escrow Manager`. An import can overwrite rules in the target folder.

To change a rule, edit the file and import it again into the same folder. Changes made in the
Grafana UI are not written back to this repository.
