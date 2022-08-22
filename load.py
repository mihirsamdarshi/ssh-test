import os

from google.cloud import bigquery


def main():
    # Construct a BigQuery client object.
    client = bigquery.Client()

    table_id = os.getenv('TABLE_ID')

    job_config = bigquery.LoadJobConfig(
        source_format=bigquery.SourceFormat.NEWLINE_DELIMITED_JSON, autodetect=True,
    )

    with open('trace.json', "rb") as source_file:
        job = client.load_table_from_file(source_file, table_id, job_config=job_config)

    job.result()  # Waits for the job to complete.

    table = client.get_table(table_id)  # Make an API request.
    print(
        "Loaded {} rows and {} columns to {}".format(
            table.num_rows, len(table.schema), table_id
        )
    )


if __name__ == '__main__':
    main()
