# Approval Workflow (Python)

Expense approval workflow using signals. The workflow submits an expense,
waits for manager approval via an external signal, then processes the result.

Matches the [Approval Workflow tutorial](https://docs.sayiir.dev/tutorials/approval-workflow/).

## Prerequisites

- Python 3.10+

## Run

```bash
pip install -r requirements.txt
python main.py
```

The example uses InMemoryBackend and sends the signal in the same script.
In production, `send_approval.py` shows how to send the signal from a separate process
using PostgresBackend.
