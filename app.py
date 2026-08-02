from pihole_monitor import create_app

app = create_app()

if __name__ == "__main__":
    # 6000ではなく6001。6000はX11用に予約されており、ブラウザ(Chrome/Firefox/Safari)が
    # 「安全でないポート」として接続を拒否する(ERR_UNSAFE_PORT)ため使えない
    app.run(host="0.0.0.0", port=6001, debug=False)
