const SERVER_URL = "http://[::1]:1234/global_config/msgfplus";

$("#test").click(function () {
    queryApi()
});

async function queryApi() {
    axios.post(SERVER_URL, {
        "menu": {
            "id": "file",
            "value": "File",
            "popup": {
                "menuitem": [
                    { "value": "New", "onclick": "CreateDoc()" },
                    { "value": "Open", "onclick": "OpenDoc()" },
                    { "value": "Save", "onclick": "SaveDoc()" }
                ]
            }
        }
    }).then((response) => {
        console.log(response.data);
    })
         .catch((error) => {
             console.log(error);
         })
}

