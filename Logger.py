import logging
from logging.handlers import RotatingFileHandler


class Logger:
    def __init__(self, name='Logger', log_file='app.log', log_termianl = False, log_level=logging.DEBUG, max_bytes=5*1024*1024, backup_count=3):
        self.logger = logging.getLogger(name)
        self.logger.setLevel(log_level)

        formatter = logging.Formatter('%(asctime)s - %(name)s - %(levelname)-8s - %(message)s')

        if log_termianl:
            console_handler = logging.StreamHandler()
            console_handler.setLevel(log_level)
            console_handler.setFormatter(formatter)
            self.logger.addHandler(console_handler)

        file_handler = RotatingFileHandler(log_file, maxBytes=max_bytes, backupCount=backup_count)
        file_handler.setLevel(log_level)
        file_handler.setFormatter(formatter)
        self.logger.addHandler(file_handler)

    def debug(self, message):
        self.logger.debug(message)

    def info(self, message):
        self.logger.info(message)

    def warning(self, message):
        self.logger.warning(message)

    def error(self, message):
        self.logger.error(message)

    def critical(self, message):
        self.logger.critical(message)

# 示例用法
if __name__ == "__main__":
    log = Logger(log_file='logger.log')
    log.debug('This is a debug message')
    log.info('This is an info message')
    log.warning('This is a warning message')
    log.error('This is an error message')
    log.critical('This is a critical message')
