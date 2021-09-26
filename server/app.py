from flask import Flask, jsonify, request
from flask_cors import CORS
from flask_restful import Api, Resource


app = Flask(__name__)
api = Api(app)
CORS(app)


class Base(Resource):
    @staticmethod
    def get():
        return jsonify('success')

    @staticmethod
    def post():
        print(request.data)
        return jsonify('success')


api.add_resource(Base, '/')

if __name__ == "__main__":
    app.run(debug=True, port=5000)