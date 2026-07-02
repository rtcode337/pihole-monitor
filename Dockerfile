FROM python:3.11-slim

WORKDIR /app

RUN pip install flask requests --no-cache-dir

COPY app.py .

CMD ["python", "app.py"]
